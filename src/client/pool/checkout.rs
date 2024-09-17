use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;

use std::task::ready;
use std::task::Context;
use std::task::Poll;

use futures_util::future::BoxFuture;
use pin_project::pin_project;
use pin_project::pinned_drop;
use thiserror::Error;
use tokio::sync::oneshot::Receiver;
use tracing::debug;
use tracing::trace;

use super::Key;
use super::Pool;
use super::PoolRef;
use super::PoolableConnection;
use super::PoolableTransport;
use super::Pooled;

#[cfg(debug_assertions)]
static CHECKOUT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

#[cfg(debug_assertions)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CheckoutId(usize);

#[cfg(debug_assertions)]
impl CheckoutId {
    fn new() -> Self {
        CheckoutId(CHECKOUT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
    }
}

#[cfg(debug_assertions)]
impl fmt::Display for CheckoutId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "checkout-{}", self.0)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error<E> {
    #[error("creating connection")]
    Connecting(#[source] E),

    #[error("handshaking connection")]
    Handshaking(#[source] E),

    #[error("connection closed")]
    Unavailable,
}

#[pin_project(project = WaitingProjected)]
pub(crate) enum Waiting<C: PoolableConnection> {
    /// The checkout is waiting on an idle connection, and should
    /// attempt its own connection in the interim as well.
    Idle(#[pin] Receiver<Pooled<C>>),

    /// The checkout is waiting on a connection currently in the process
    /// of connecting, and should wait for that connection to complete,
    /// not starting its own connection.
    Connecting(#[pin] Receiver<Pooled<C>>),

    /// There is no pool for connections to wait for.
    NoPool,
}

impl<C: PoolableConnection> Waiting<C> {
    fn close(&mut self) {
        match self {
            Waiting::Idle(rx) => {
                rx.close();
            }
            Waiting::Connecting(rx) => {
                rx.close();
            }
            Waiting::NoPool => {}
        }

        *self = Waiting::NoPool;
    }
}

impl<C: PoolableConnection> fmt::Debug for Waiting<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Waiting::Idle(_) => f.debug_tuple("Idle").finish(),
            Waiting::Connecting(_) => f.debug_tuple("Connecting").finish(),
            Waiting::NoPool => f.debug_tuple("NoPool").finish(),
        }
    }
}

pub(crate) enum WaitingPoll<C: PoolableConnection> {
    Connected(Pooled<C>),
    Closed,
    NotReady,
}

impl<C: PoolableConnection> Future for Waiting<C> {
    type Output = WaitingPoll<C>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project() {
            WaitingProjected::Idle(rx) => match rx.poll(cx) {
                Poll::Ready(Ok(connection)) => Poll::Ready(WaitingPoll::Connected(connection)),
                Poll::Ready(Err(_)) => Poll::Ready(WaitingPoll::Closed),
                Poll::Pending => Poll::Ready(WaitingPoll::NotReady),
            },
            WaitingProjected::Connecting(rx) => match rx.poll(cx) {
                Poll::Ready(Ok(connection)) => Poll::Ready(WaitingPoll::Connected(connection)),
                Poll::Ready(Err(_)) => Poll::Ready(WaitingPoll::Closed),
                Poll::Pending => Poll::Pending,
            },
            WaitingProjected::NoPool => Poll::Ready(WaitingPoll::Closed),
        }
    }
}

/// A connector combines the futures required to connect to a transport
/// and then complete the transport's associated startup handshake.
pub struct Connector<C: PoolableConnection, T: PoolableTransport, E> {
    transport: BoxFuture<'static, Result<T, E>>,
    handshake: Box<dyn FnOnce(T) -> BoxFuture<'static, Result<C, E>> + Send>,
}

impl<C: PoolableConnection, T: PoolableTransport, E> fmt::Debug for Connector<C, T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connector").finish()
    }
}

impl<C: PoolableConnection, T: PoolableTransport, E> Connector<C, T, E> {
    /// Create a new connection from a transport connector and a handshake function.
    pub fn new<R, F, H>(transport: F, handshake: H) -> Self
    where
        H: FnOnce(T) -> BoxFuture<'static, Result<C, E>> + Send + 'static,
        R: Future<Output = Result<T, E>> + Send + 'static,
        F: FnOnce() -> R + Send + 'static,
    {
        Self {
            transport: Box::pin(crate::lazy::lazy(transport)),
            handshake: Box::new(handshake),
        }
    }
}

pub(crate) enum InnerCheckoutConnecting<C: PoolableConnection, T: PoolableTransport, E> {
    Waiting,
    Connected,
    Connecting(Connector<C, T, E>),
    Handshaking(BoxFuture<'static, Result<C, E>>),
}

impl<C: PoolableConnection, T: PoolableTransport, E> fmt::Debug
    for InnerCheckoutConnecting<C, T, E>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InnerCheckoutConnecting::Waiting => f.debug_tuple("Waiting").finish(),
            InnerCheckoutConnecting::Connected => f.debug_tuple("Connected").finish(),
            InnerCheckoutConnecting::Connecting(_) => f.debug_tuple("Connecting").finish(),
            InnerCheckoutConnecting::Handshaking(_) => f.debug_tuple("Handshaking").finish(),
        }
    }
}

#[pin_project(PinnedDrop)]
pub(crate) struct Checkout<C: PoolableConnection, T: PoolableTransport, E: 'static> {
    key: Key,
    pool: PoolRef<C>,
    #[pin]
    waiter: Waiting<C>,
    inner: InnerCheckoutConnecting<C, T, E>,
    connection: Option<C>,
    connection_error: PhantomData<fn() -> E>,
    delayed_drop: bool,
    #[cfg(debug_assertions)]
    id: CheckoutId,
}

impl<C: PoolableConnection, T: PoolableTransport, E: 'static> fmt::Debug for Checkout<C, T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Checkout")
            .field("key", &self.key)
            .field("pool", &self.pool)
            .field("waiter", &self.waiter)
            .field("inner", &self.inner)
            .finish()
    }
}

impl<C: PoolableConnection, T: PoolableTransport, E: 'static> Checkout<C, T, E> {
    /// Returns a new checkout which takes over ownership of the in-progress connection.
    fn as_detached(self: Pin<&mut Self>) -> Self {
        let this = self.project();

        #[cfg(debug_assertions)]
        tracing::trace!(%this.key, %this.id, "detaching checkout");

        Checkout {
            key: this.key.clone(),
            pool: this.pool.clone(),
            waiter: Waiting::NoPool,
            inner: std::mem::replace(this.inner, InnerCheckoutConnecting::Connected),
            connection: None,
            connection_error: PhantomData,
            delayed_drop: false,
            #[cfg(debug_assertions)]
            id: *this.id,
        }
    }

    pub(crate) fn detached(key: Key, connector: Connector<C, T, E>) -> Self {
        #[cfg(debug_assertions)]
        let id = CheckoutId::new();

        #[cfg(debug_assertions)]
        tracing::trace!(%key, %id, "creating detached checkout");

        Self {
            key,
            pool: PoolRef::none(),
            waiter: Waiting::NoPool,
            inner: InnerCheckoutConnecting::Connecting(connector),
            connection: None,
            connection_error: PhantomData,
            delayed_drop: false,
            #[cfg(debug_assertions)]
            id,
        }
    }

    pub(super) fn new(
        key: Key,
        pool: &Pool<C>,
        waiter: Receiver<Pooled<C>>,
        connect: Option<Connector<C, T, E>>,
        connection: Option<C>,
        continue_after_premeption: bool,
    ) -> Self {
        #[cfg(debug_assertions)]
        let id = CheckoutId::new();

        #[cfg(debug_assertions)]
        tracing::trace!(%key, %id, "creating new checkout");

        if connection.is_some() {
            tracing::trace!(%key, "connection recieved from pool");
            Self {
                key,
                pool: pool.as_ref(),
                waiter: Waiting::Idle(waiter),
                inner: InnerCheckoutConnecting::Connected,
                connection,
                connection_error: PhantomData,
                delayed_drop: false,
                #[cfg(debug_assertions)]
                id,
            }
        } else if let Some(connector) = connect {
            tracing::trace!(%key, "connecting to pool");
            Self {
                key,
                pool: pool.as_ref(),
                waiter: Waiting::Idle(waiter),
                inner: InnerCheckoutConnecting::Connecting(connector),
                connection,
                connection_error: PhantomData,
                delayed_drop: continue_after_premeption,
                #[cfg(debug_assertions)]
                id,
            }
        } else {
            tracing::trace!(%key, "waiting for connection");
            Self {
                key,
                pool: pool.as_ref(),
                waiter: Waiting::Connecting(waiter),
                inner: InnerCheckoutConnecting::Waiting,
                connection,
                connection_error: PhantomData,
                delayed_drop: false,
                #[cfg(debug_assertions)]
                id,
            }
        }
    }
}

impl<C, T, E> Future for Checkout<C, T, E>
where
    C: PoolableConnection,
    T: PoolableTransport,
    E: 'static,
{
    type Output = Result<Pooled<C>, Error<E>>;

    #[cfg_attr(debug_assertions, tracing::instrument("checkout::poll", skip_all, fields(id=%self.id), level="trace"))]
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Outcomes from .poll_waiter:
        // - Ready(Some(connection)) => return connection
        // - Ready(None) => continue to check pool, we don't have a waiter.
        // - Pending => wait on the waiter to complete, don't bother to check pool.

        // Open questions: Should we check the pool for a different connection when the
        // waiter is pending? Probably not, ideally our semantics should keep the pool
        // from containing multiple connections if they can be multiplexed.

        if let WaitingPoll::Connected(connection) = ready!(self.as_mut().poll_waiter(cx)) {
            debug!(key=%self.key, "connection recieved from waiter");
            return Poll::Ready(Ok(connection));
        }

        trace!(key=%self.key, "polling for new connection");
        // Try to connect while we also wait for a checkout to be ready.
        loop {
            let this = self.as_mut().project();
            let transport: T = match this.inner {
                InnerCheckoutConnecting::Waiting => {
                    // We're waiting on a connection to be ready.
                    // If that were still happening, we would bail out above.
                    return Poll::Ready(Err(Error::Unavailable));
                }
                InnerCheckoutConnecting::Connected => {
                    // We've already connected, we can just return the connection.
                    let connection = this
                        .connection
                        .take()
                        .expect("future was polled after completion");

                    return Poll::Ready(Ok(self.as_mut().connected(connection)));
                }
                InnerCheckoutConnecting::Connecting(Connector { transport, .. }) => {
                    ready!(transport.as_mut().poll(cx)).map_err(Error::Connecting)?
                }
                InnerCheckoutConnecting::Handshaking(handshake) => {
                    let connection =
                        ready!(handshake.as_mut().poll(cx)).map_err(Error::Handshaking)?;
                    *this.inner = InnerCheckoutConnecting::Connected;
                    return Poll::Ready(Ok(self.as_mut().connected(connection)));
                }
            };

            if transport.can_share() {
                // This can happen if we connect expecting an HTTP/1.1 connection, but during the TLS
                // handshake we discover that the connection is actually an HTTP/2 connection.
                trace!(key=%this.key, "connection can be shared, telling pool to wait for handshake");
                if let Some(mut pool) = this.pool.lock() {
                    pool.connected_in_handshake(this.key);
                }
            }

            // Destructure the InnerCheckoutConnecting to get the handshake function.
            let InnerCheckoutConnecting::Connecting(Connector {
                transport: _,
                handshake,
            }) = std::mem::replace(this.inner, InnerCheckoutConnecting::Waiting)
            else {
                unreachable!()
            };

            *this.inner = InnerCheckoutConnecting::Handshaking(Box::pin(handshake(transport)));
        }
    }
}

impl<C: PoolableConnection, T: PoolableTransport, E: 'static> Checkout<C, T, E> {
    /// Checks the waiter to see if a new connection is ready and can be passed along.
    ///
    /// If there is no waiter, this function returns `Poll::Ready(Ok(None))`. If there is
    /// a waiter, then it will poll the waiter and return the result.
    pub(crate) fn poll_waiter(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<WaitingPoll<C>> {
        let this = self.project();

        trace!(key=%this.key, "polling for waiter");
        this.waiter.poll(cx)
    }

    /// Called to register a new connection with the pool.
    pub(crate) fn connected(mut self: Pin<&mut Self>, connection: C) -> Pooled<C> {
        self.waiter.close();
        register_connected(&self.pool, &self.key, connection)
    }
}

fn register_connected<C>(poolref: &PoolRef<C>, key: &Key, mut connection: C) -> Pooled<C>
where
    C: PoolableConnection,
{
    if let Some(mut pool) = poolref.lock() {
        if let Some(reused) = connection.reuse() {
            pool.push(key.clone(), reused, poolref.clone());
            return Pooled {
                connection: Some(connection),
                is_reused: true,
                key: key.clone(),
                pool: PoolRef::none(),
            };
        } else {
            return Pooled {
                connection: Some(connection),
                is_reused: false,
                key: key.clone(),
                pool: poolref.clone(),
            };
        }
    }

    // No pool or lock was available, so we can't add the connection to the pool.
    Pooled {
        connection: Some(connection),
        is_reused: false,
        key: key.clone(),
        pool: PoolRef::none(),
    }
}

#[pinned_drop]
impl<C: PoolableConnection, T: PoolableTransport, E> PinnedDrop for Checkout<C, T, E>
where
    E: 'static,
{
    fn drop(mut self: Pin<&mut Self>) {
        #[cfg(debug_assertions)]
        tracing::trace!(id=%self.id, "drop for checkout");

        if let Some(mut pool) = self.pool.lock() {
            pool.cancel_connection(&self.key);

            // If we are still connecting, we can detach and finish that elsewhere
            if self.delayed_drop
                && !matches!(
                    self.inner,
                    InnerCheckoutConnecting::Connected | InnerCheckoutConnecting::Waiting,
                )
            {
                trace!(key=%self.key, state=?self.inner, "delaying connection drop");
                let checkout = self.as_detached();
                tokio::spawn(async move {
                    let key = checkout.key.clone();
                    if let Err(error) = checkout.await {
                        debug!(%key, "background connection failed to connect: {}", error);
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use static_assertions::assert_impl_all;

    assert_impl_all!(Error<std::io::Error>: std::error::Error, Send, Sync, Into<Box<dyn std::error::Error + Send + Sync>>);

    #[cfg(feature = "mocks")]
    use crate::client::conn::transport::mock::MockTransport;

    #[test]
    fn verify_checkout_id() {
        let id = CheckoutId(0);
        assert_eq!(id.to_string(), "checkout-0");
        assert_eq!(id, CheckoutId(0));
        assert_eq!(format!("{:?}", id), "CheckoutId(0)");
        assert_eq!(id.clone(), CheckoutId(0));
    }

    #[cfg(feature = "mocks")]
    #[tokio::test]
    async fn detatched_checkout() {
        let key: Key = "http://localhost:8080".parse().unwrap();

        let transport = MockTransport::single();

        let checkout = Checkout::detached(key, transport.connector());

        let dbg = format!("{:?}", checkout);
        assert_eq!(
            dbg,
            "Checkout { key: Key(\"http\", Some(localhost:8080)), pool: PoolRef(WeakOpt(None)), waiter: NoPool, inner: Connecting }"
        );

        let connection = checkout.await.unwrap();
        assert!(connection.is_open());
    }
}
