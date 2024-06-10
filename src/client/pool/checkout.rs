use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::ready;
use std::task::Context;
use std::task::Poll;

use futures_util::future::BoxFuture;
use futures_util::FutureExt as _;
use pin_project::pin_project;
use pin_project::pinned_drop;
use thiserror::Error;
use tokio::sync::oneshot::Receiver;
use tracing::debug;
use tracing::trace;

use super::Key;
use super::PoolInner;
use super::PoolableConnection;
use super::PoolableTransport;
use super::Pooled;
use super::WeakOpt;

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

#[derive(Debug, Error)]
pub enum Error<E> {
    Connecting(#[source] E),
    Handshaking(#[source] E),
    Unavailable,
}

#[pin_project(project = WaitingProjected)]
pub(crate) enum Waiting<C: PoolableConnection> {
    Idle(#[pin] Receiver<Pooled<C>>),
    Connecting(#[pin] Receiver<Pooled<C>>),
    NoPool,
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

pub(crate) struct Connector<C: PoolableConnection, T: PoolableTransport, E> {
    transport: BoxFuture<'static, Result<T, E>>,
    handshake: Box<dyn FnOnce(T) -> BoxFuture<'static, Result<C, E>> + Send>,
}

impl<C: PoolableConnection, T: PoolableTransport, E> fmt::Debug for Connector<C, T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connector").finish()
    }
}

impl<C: PoolableConnection, T: PoolableTransport, E> Connector<C, T, E> {
    pub(crate) fn new<R, F, H>(transport: F, handshake: H) -> Self
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
pub(crate) struct Checkout<C: PoolableConnection, T: PoolableTransport, E> {
    key: Key,
    pool: WeakOpt<Mutex<PoolInner<C>>>,
    #[pin]
    waiter: Waiting<C>,
    inner: InnerCheckoutConnecting<C, T, E>,
    connection: Option<C>,
    connection_error: PhantomData<fn() -> E>,
    #[cfg(debug_assertions)]
    id: CheckoutId,
}

impl<C: PoolableConnection, T: PoolableTransport, E> fmt::Debug for Checkout<C, T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Checkout")
            .field("key", &self.key)
            .field("pool", &self.pool)
            .field("waiter", &self.waiter)
            .field("inner", &self.inner)
            .finish()
    }
}

impl<C: PoolableConnection, T: PoolableTransport, E> Checkout<C, T, E> {
    pub(crate) fn detached(key: Key, connector: Connector<C, T, E>) -> Self {
        Self {
            key,
            pool: WeakOpt::none(),
            waiter: Waiting::NoPool,
            inner: InnerCheckoutConnecting::Connecting(connector),
            connection: None,
            connection_error: PhantomData,
            #[cfg(debug_assertions)]
            id: CheckoutId::new(),
        }
    }

    pub(super) fn new(
        key: Key,
        pool: &Arc<Mutex<PoolInner<C>>>,
        waiter: Receiver<Pooled<C>>,
        connect: Option<Connector<C, T, E>>,
        connection: Option<C>,
    ) -> Self {
        #[cfg(debug_assertions)]
        let id = CheckoutId::new();

        let pool = WeakOpt::downgrade(pool);
        if connection.is_some() {
            tracing::debug!(key=%key, "connection recieved from pool");
            Self {
                key,
                pool,
                waiter: Waiting::Idle(waiter),
                inner: InnerCheckoutConnecting::Connected,
                connection,
                connection_error: PhantomData,
                #[cfg(debug_assertions)]
                id,
            }
        } else if let Some(connector) = connect {
            Self {
                key,
                pool,
                waiter: Waiting::Idle(waiter),
                inner: InnerCheckoutConnecting::Connecting(connector),
                connection,
                connection_error: PhantomData,
                #[cfg(debug_assertions)]
                id,
            }
        } else {
            Self {
                key,
                pool,
                waiter: Waiting::Connecting(waiter),
                inner: InnerCheckoutConnecting::Waiting,
                connection,
                connection_error: PhantomData,
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
                    ready!(transport.poll_unpin(cx)).map_err(Error::Connecting)?
                }
                InnerCheckoutConnecting::Handshaking(handshake) => {
                    let connection =
                        ready!(handshake.poll_unpin(cx)).map_err(Error::Handshaking)?;
                    return Poll::Ready(Ok(self.as_mut().connected(connection)));
                }
            };

            if transport.can_share() {
                trace!(key=%this.key, "connection can be shared");
                if let Some(pool) = this.pool.upgrade() {
                    if let Ok(mut inner) = pool.lock() {
                        inner.connected_in_handshake(this.key);
                    }
                }
            }

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

impl<C: PoolableConnection, T: PoolableTransport, E> Checkout<C, T, E> {
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
    pub(crate) fn connected(self: Pin<&mut Self>, mut connection: C) -> Pooled<C> {
        if let Some(pool) = self.pool.upgrade() {
            if let Ok(mut inner) = pool.lock() {
                if let Some(reused) = connection.reuse() {
                    inner.push(self.key.clone(), reused, self.pool.clone());
                    return Pooled {
                        connection: Some(connection),
                        is_reused: true,
                        key: self.key.clone(),
                        pool: WeakOpt::none(),
                    };
                } else {
                    return Pooled {
                        connection: Some(connection),
                        is_reused: false,
                        key: self.key.clone(),
                        pool: WeakOpt::downgrade(&pool),
                    };
                }
            }
        }

        // No pool or lock was available, so we can't add the connection to the pool.
        Pooled {
            connection: Some(connection),
            is_reused: false,
            key: self.key.clone(),
            pool: WeakOpt::none(),
        }
    }
}

#[pinned_drop]
impl<C: PoolableConnection, T: PoolableTransport, E> PinnedDrop for Checkout<C, T, E> {
    fn drop(self: Pin<&mut Self>) {
        if let Some(pool) = self.pool.upgrade() {
            if let Ok(mut inner) = pool.lock() {
                inner.cancel_connection(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::super::mock::MockTransport;
    use super::*;

    #[test]
    fn verify_checkout_id() {
        let id = CheckoutId(0);
        assert_eq!(id.to_string(), "checkout-0");
        assert_eq!(id, CheckoutId(0));
        assert_eq!(format!("{:?}", id), "CheckoutId(0)");
        assert_eq!(id.clone(), CheckoutId(0));
    }

    #[tokio::test]
    async fn detatched_checkout() {
        let key: Key = "http://localhost:8080".parse().unwrap();

        let checkout = Checkout::detached(
            key,
            Connector::new(MockTransport::single, MockTransport::handshake),
        );

        let dbg = format!("{:?}", checkout);
        assert_eq!(
            dbg,
            "Checkout { key: Key(\"http\", localhost:8080), pool: WeakOpt(None), waiter: NoPool, inner: Connecting }"
        );

        let connection = checkout.await.unwrap();
        assert!(connection.is_open());
    }
}
