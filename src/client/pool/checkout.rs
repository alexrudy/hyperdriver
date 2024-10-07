use std::fmt;
use std::future::Future;
use std::pin::Pin;

use std::task::ready;
use std::task::Context;
use std::task::Poll;

use http::Uri;
use hyper::rt::Executor;
use pin_project::pin_project;
use pin_project::pinned_drop;
use thiserror::Error;
use tokio::sync::oneshot::Receiver;
use tracing::debug;
use tracing::trace;

use crate::client::conn::protocol::HttpProtocol;
use crate::client::conn::Protocol;
use crate::client::conn::Transport;
use crate::info::ConnectionInfo;
use crate::info::HasConnectionInfo;

use super::Config;
use super::Key;
use super::Pool;
use super::PoolRef;
use super::PoolableConnection;
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
#[non_exhaustive]
pub enum Error<Transport, Protocol> {
    #[error("creating connection")]
    Connecting(#[source] Transport),

    #[error("handshaking connection")]
    Handshaking(#[source] Protocol),

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

#[pin_project(project = ConnectorStateProjected)]
enum ConnectorState<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
    P::Connection: PoolableConnection,
{
    PollReadyTransport {
        address: Uri,
        transport: Option<T>,
        protocol: Option<P>,
    },
    Connect {
        #[pin]
        future: T::Future,
        protocol: Option<P>,
    },
    PollReadyHandshake {
        protocol: Option<P>,
        stream: Option<T::IO>,
    },
    Handshake {
        #[pin]
        future: <P as Protocol<T::IO, B>>::Future,
        info: ConnectionInfo<<T::IO as HasConnectionInfo>::Addr>,
    },
}

impl<T, P, B> fmt::Debug for ConnectorState<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
    P::Connection: PoolableConnection,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectorState::PollReadyTransport { address, .. } => f
                .debug_struct("PollReadyTransport")
                .field("address", address)
                .finish(),
            ConnectorState::Connect { .. } => f.debug_tuple("Connect").finish(),
            ConnectorState::PollReadyHandshake { .. } => {
                f.debug_tuple("PollReadyHandshake").finish()
            }
            ConnectorState::Handshake { .. } => f.debug_tuple("Handshake").finish(),
        }
    }
}

/// A connector combines the futures required to connect to a transport
/// and then complete the transport's associated startup handshake.
#[pin_project]
pub struct Connector<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
    P::Connection: PoolableConnection,
{
    #[pin]
    state: ConnectorState<T, P, B>,

    version: Option<HttpProtocol>,
    shareable: bool,
}

impl<T, P, B> fmt::Debug for Connector<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
    P::Connection: PoolableConnection,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connector")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl<T, P, B> Connector<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
    P::Connection: PoolableConnection,
{
    /// Create a new connection from a transport connector and a handshake function.
    pub fn new(transport: T, protocol: P, address: Uri, version: HttpProtocol) -> Self {
        //TODO: Fix this
        let shareable = false;

        Self {
            state: ConnectorState::PollReadyTransport {
                address,
                transport: Some(transport),
                protocol: Some(protocol),
            },
            version: Some(version),
            shareable,
        }
    }
}

#[allow(type_alias_bounds)]
type ConnectionError<T: Transport, P: Protocol<T::IO, B>, B> =
    Error<<T as Transport>::Error, <P as Protocol<T::IO, B>>::Error>;

impl<T, P, B> Connector<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
    P::Connection: PoolableConnection,
{
    fn poll_connector(
        self: Pin<&mut Self>,
        pool: &PoolRef<P::Connection>,
        key: &Key,
        meta: &mut CheckoutMeta,
        cx: &mut Context<'_>,
    ) -> Poll<Result<P::Connection, ConnectionError<T, P, B>>> {
        let mut connector_projected = self.project();

        loop {
            match connector_projected.state.as_mut().project() {
                ConnectorStateProjected::PollReadyTransport {
                    address,
                    transport,
                    protocol,
                } => {
                    let _entered = meta.transport().enter();
                    {
                        let transport = transport.as_mut().unwrap();
                        if let Err(error) = ready!(transport.poll_ready(cx)) {
                            return Poll::Ready(Err(Error::Connecting(error)));
                        }
                    }

                    let mut transport = transport
                        .take()
                        .expect("future polled in invalid state: transport is None");
                    let future = transport.connect(address.clone());
                    let protocol = protocol.take();

                    tracing::trace!("transport ready");
                    connector_projected
                        .state
                        .set(ConnectorState::Connect { future, protocol });
                }

                ConnectorStateProjected::Connect { future, protocol } => {
                    let _entered = meta.transport().enter();
                    let stream = match ready!(future.poll(cx)) {
                        Ok(stream) => stream,
                        Err(error) => return Poll::Ready(Err(Error::Connecting(error))),
                    };
                    let protocol = protocol.take();

                    tracing::trace!("transport connected");
                    connector_projected
                        .state
                        .set(ConnectorState::PollReadyHandshake {
                            protocol,
                            stream: Some(stream),
                        });
                }

                ConnectorStateProjected::PollReadyHandshake { protocol, stream } => {
                    let _entered = meta.protocol().enter();

                    {
                        let protocol = protocol.as_mut().unwrap();
                        if let Err(error) =
                            ready!(<P as Protocol<T::IO, B>>::poll_ready(protocol, cx))
                        {
                            return Poll::Ready(Err(Error::Handshaking(error)));
                        }
                    }

                    let stream = stream
                        .take()
                        .expect("future polled in invalid state: stream is None");

                    let info = stream.info();

                    let future = protocol
                        .as_mut()
                        .expect("future polled in invalid state: protocol is None")
                        .connect(
                            stream,
                            connector_projected.version.take().expect("version is None"),
                        );

                    if *connector_projected.shareable {
                        // This can happen if we connect expecting an HTTP/1.1 connection, but during the TLS
                        // handshake we discover that the connection is actually an HTTP/2 connection.
                        trace!(key=%key, "connection can be shared, telling pool to wait for handshake");
                        if let Some(mut pool) = pool.lock() {
                            pool.connected_in_handshake(key);
                        }
                    }

                    tracing::trace!("handshake ready");

                    connector_projected
                        .state
                        .set(ConnectorState::Handshake { future, info });
                }

                ConnectorStateProjected::Handshake { future, info } => {
                    let _entered = meta.protocol().enter();

                    return future.poll(cx).map(|result| match result {
                        Ok(conn) => {
                            tracing::debug!("connection to {} ready", info.remote_addr());
                            Ok(conn)
                        }
                        Err(error) => Err(Error::Handshaking(error)),
                    });
                }
            }
        }
    }
}

#[pin_project(project = CheckoutConnectingProj, project_ref = CheckoutConnectingProjRef)]
pub(crate) enum InnerCheckoutConnecting<T, P, B, E>
where
    T: Transport,
    P: Protocol<T::IO, B>,
    P::Connection: PoolableConnection,
{
    Waiting,
    Connected,
    Connecting(#[pin] Connector<T, P, B>),
    ConnectingWithDelayDrop {
        connector: Option<Pin<Box<Connector<T, P, B>>>>,
        executor: E,
    },
}

impl<T, P, B, E> fmt::Debug for InnerCheckoutConnecting<T, P, B, E>
where
    T: Transport,
    P: Protocol<T::IO, B>,
    P::Connection: PoolableConnection,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self {
            InnerCheckoutConnecting::Waiting => f.debug_tuple("Waiting").finish(),
            InnerCheckoutConnecting::Connected => f.debug_tuple("Connected").finish(),
            InnerCheckoutConnecting::Connecting(connector) => {
                f.debug_tuple("Connecting").field(connector).finish()
            }
            InnerCheckoutConnecting::ConnectingWithDelayDrop { connector, .. } => f
                .debug_tuple("ConnectingWithDelayDrop")
                .field(connector)
                .finish(),
        }
    }
}

struct CheckoutMeta {
    overall_span: tracing::Span,
    transport_span: Option<tracing::Span>,
    protocol_span: Option<tracing::Span>,
}

impl CheckoutMeta {
    fn new() -> Self {
        let overall_span = tracing::Span::current();

        Self {
            overall_span,
            transport_span: None,
            protocol_span: None,
        }
    }

    fn transport(&mut self) -> &tracing::Span {
        self.transport_span
            .get_or_insert_with(|| tracing::trace_span!(parent: &self.overall_span, "transport"))
    }

    fn protocol(&mut self) -> &tracing::Span {
        self.protocol_span
            .get_or_insert_with(|| tracing::trace_span!(parent: &self.overall_span, "protocol"))
    }
}

#[pin_project(PinnedDrop)]
pub(crate) struct Checkout<T, P, B, E>
where
    T: Transport + 'static,
    P: Protocol<T::IO, B> + 'static,
    P::Connection: PoolableConnection,
    B: 'static,
    E: Executor<DelayedCheckout<T, P, B>>,
{
    key: Key,
    pool: PoolRef<P::Connection>,
    #[pin]
    waiter: Waiting<P::Connection>,
    #[pin]
    inner: InnerCheckoutConnecting<T, P, B, E>,
    connection: Option<P::Connection>,
    meta: CheckoutMeta,
    #[cfg(debug_assertions)]
    id: CheckoutId,
}

impl<T, P, B, E> fmt::Debug for Checkout<T, P, B, E>
where
    T: Transport + 'static,
    P: Protocol<T::IO, B> + 'static,
    P::Connection: PoolableConnection,
    B: 'static,
    E: Executor<DelayedCheckout<T, P, B>>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Checkout")
            .field("key", &self.key)
            .field("pool", &self.pool)
            .field("waiter", &self.waiter)
            .field("inner", &self.inner)
            .finish()
    }
}

impl<T, P, B, E, C> Checkout<T, P, B, E>
where
    T: Transport + 'static,
    P: Protocol<T::IO, B, Connection = C> + 'static,
    P::Connection: PoolableConnection,
    B: 'static,
    E: Executor<DelayedCheckout<T, P, B>>,
{
    /// Converts this checkout into a "delayed drop" checkout.
    fn spawn_delayed(self: Pin<&mut Self>) -> bool {
        let mut this = self.project();

        match this.inner.as_mut().project() {
            CheckoutConnectingProj::ConnectingWithDelayDrop {
                connector,
                executor,
            } if connector.is_some() => {
                tracing::trace!("converting checkout to delayed drop");
                let delayed = DelayedCheckout {
                    key: this.key.clone(),
                    pool: this.pool.clone(),
                    meta: CheckoutMeta::new(),
                    connector: connector.take().unwrap(),
                };

                executor.execute(delayed);
                true
            }
            _ => false,
        }
    }

    /// Constructs a checkout which does not hold a reference to the pool
    /// and so is only waiting on the connector.
    ///
    /// This checkout will always proceed with the connector, uninterrupted by
    /// alternative connection solutions. It will not use the "delayed drop"
    /// procedure to finish connections if dropped.
    ///
    /// This is useful when using a checkout to poll a connection to readiness
    /// without a pool, or in a context in which the associated connection cannot
    /// or should not be shared with the pool.
    pub(crate) fn detached(key: Key, connector: Connector<T, P, B>) -> Self {
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
            meta: CheckoutMeta::new(),
            #[cfg(debug_assertions)]
            id,
        }
    }

    pub(super) fn connected(
        key: Key,
        pool: &Pool<P::Connection>,
        connection: P::Connection,
    ) -> Self {
        #[cfg(debug_assertions)]
        let id = CheckoutId::new();
        let meta = CheckoutMeta::new();

        #[cfg(debug_assertions)]
        tracing::trace!(%key, %id, "connection recieved from pool");

        Self {
            key,
            pool: pool.as_ref(),
            waiter: Waiting::NoPool,
            inner: InnerCheckoutConnecting::Connected,
            connection: Some(connection),
            meta,
            #[cfg(debug_assertions)]
            id,
        }
    }

    pub(super) fn connecting(
        key: Key,
        pool: &Pool<P::Connection>,
        waiter: Receiver<Pooled<P::Connection>>,
        connector: Connector<T, P, B>,
        config: &Config,
        executor: E,
    ) -> Self {
        #[cfg(debug_assertions)]
        let id = CheckoutId::new();
        let meta = CheckoutMeta::new();

        #[cfg(debug_assertions)]
        tracing::trace!(%key, %id, "creating new checkout");

        tracing::trace!(%key, "connecting with pool");

        let inner = if config.continue_after_preemption {
            InnerCheckoutConnecting::ConnectingWithDelayDrop {
                connector: Some(Box::pin(connector)),
                executor,
            }
        } else {
            InnerCheckoutConnecting::Connecting(connector)
        };

        Self {
            key,
            pool: pool.as_ref(),
            waiter: Waiting::Idle(waiter),
            inner,
            connection: None,
            meta,
            #[cfg(debug_assertions)]
            id,
        }
    }

    pub(super) fn waiting(
        key: Key,
        pool: &Pool<P::Connection>,
        waiter: Receiver<Pooled<P::Connection>>,
    ) -> Self {
        #[cfg(debug_assertions)]
        let id = CheckoutId::new();
        let meta = CheckoutMeta::new();

        #[cfg(debug_assertions)]
        tracing::trace!(%key, %id, "creating new checkout");

        Self {
            key,
            pool: pool.as_ref(),
            waiter: Waiting::Connecting(waiter),
            inner: InnerCheckoutConnecting::Waiting,
            connection: None,
            meta,
            #[cfg(debug_assertions)]
            id,
        }
    }
}

impl<T, P, B, E> Future for Checkout<T, P, B, E>
where
    T: Transport + 'static,
    P: Protocol<T::IO, B> + 'static,
    P::Connection: PoolableConnection,
    B: 'static,
    E: Executor<DelayedCheckout<T, P, B>>,
{
    type Output = Result<
        Pooled<P::Connection>,
        Error<<T as Transport>::Error, <P as Protocol<T::IO, B>>::Error>,
    >;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.as_mut().project();
        let _entered = this.meta.overall_span.clone().entered();

        {
            // Outcomes from .poll_waiter:
            // - Ready(Some(connection)) => return connection
            // - Ready(None) => continue to check pool, we don't have a waiter.
            // - Pending => wait on the waiter to complete, don't bother to check pool.

            // Open questions: Should we check the pool for a different connection when the
            // waiter is pending? Probably not, ideally our semantics should keep the pool
            // from containing multiple connections if they can be multiplexed.
            if let WaitingPoll::Connected(connection) = ready!(this.waiter.as_mut().poll(cx)) {
                debug!(key=%this.key, "connection recieved from waiter");

                return Poll::Ready(Ok(connection));
            }
        }

        trace!(key=%this.key, "polling for new connection");
        // Try to connect while we also wait for a checkout to be ready.

        match this.inner.as_mut().project() {
            CheckoutConnectingProj::Waiting => {
                // We're waiting on a connection to be ready.
                // If that were still happening, we would bail out above, since the waiter
                // would return Poll::Pending.
                Poll::Ready(Err(Error::Unavailable))
            }
            CheckoutConnectingProj::Connected => {
                // We've already connected, we can just return the connection.
                let connection = this
                    .connection
                    .take()
                    .expect("future was polled after completion");

                this.waiter.close();
                this.inner.set(InnerCheckoutConnecting::Connected);
                Poll::Ready(Ok(register_connected(this.pool, this.key, connection)))
            }
            CheckoutConnectingProj::Connecting(connector) => {
                let result = ready!(connector.poll_connector(this.pool, this.key, this.meta, cx));

                this.waiter.close();
                this.inner.set(InnerCheckoutConnecting::Connected);

                match result {
                    Ok(connection) => {
                        Poll::Ready(Ok(register_connected(this.pool, this.key, connection)))
                    }
                    Err(e) => Poll::Ready(Err(e)),
                }
            }
            CheckoutConnectingProj::ConnectingWithDelayDrop {
                connector: Some(connector),
                ..
            } => {
                let result = ready!(connector
                    .as_mut()
                    .poll_connector(this.pool, this.key, this.meta, cx));

                this.waiter.close();
                this.inner.set(InnerCheckoutConnecting::Connected);

                match result {
                    Ok(connection) => {
                        Poll::Ready(Ok(register_connected(this.pool, this.key, connection)))
                    }
                    Err(e) => Poll::Ready(Err(e)),
                }
            }
            CheckoutConnectingProj::ConnectingWithDelayDrop {
                connector: None, ..
            } => {
                // Something stole our connection, this is an error state.
                panic!("connection was stolen from checkout")
            }
        }
    }
}

/// Register a connection with the pool referenced here.
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
impl<T, P, B, E> PinnedDrop for Checkout<T, P, B, E>
where
    T: Transport + 'static,
    P: Protocol<T::IO, B> + 'static,
    P::Connection: PoolableConnection,
    B: 'static,
    E: Executor<DelayedCheckout<T, P, B>>,
{
    fn drop(mut self: Pin<&mut Self>) {
        #[cfg(debug_assertions)]
        tracing::trace!(id=%self.id, "drop for checkout");

        if self.as_mut().spawn_delayed() {
        } else if let Some(mut pool) = self.pool.lock() {
            // Connection is only cancled when no delayed drop occurs.
            pool.cancel_connection(&self.key);
        }
    }
}

/// Future that will be spawned to delay the drop of a checkout.
///
/// This allows a pre-empted, in-progress checkout to complete,
/// adding a new connection to the pool if it succeeds. This way,
/// the work done to partially complete a handshake is not wasted.
pub struct DelayedCheckout<T, P, B>
where
    T: Transport + 'static,
    P: Protocol<T::IO, B> + 'static,
    P::Connection: PoolableConnection,
    B: 'static,
{
    key: Key,
    pool: PoolRef<P::Connection>,
    meta: CheckoutMeta,
    connector: Pin<Box<Connector<T, P, B>>>,
}

impl<T, P, B> fmt::Debug for DelayedCheckout<T, P, B>
where
    T: Transport + 'static,
    P: Protocol<T::IO, B> + 'static,
    P::Connection: PoolableConnection,
    B: 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DelayedCheckout")
            .field(&self.connector)
            .finish()
    }
}

impl<T, P, B> Future for DelayedCheckout<T, P, B>
where
    T: Transport + 'static,
    P: Protocol<T::IO, B> + 'static,
    P::Connection: PoolableConnection,
    B: 'static,
{
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = Pin::into_inner(self.as_mut());

        match ready!(this.connector.as_mut().poll_connector(
            &this.pool,
            &this.key,
            &mut this.meta,
            cx
        )) {
            Ok(connection) => {
                if let Some(mut pool) = self.pool.lock() {
                    pool.push(self.key.clone(), connection, self.pool.clone());
                }
            }
            Err(err) => {
                tracing::error!(error=%err, "error during delayed drop");
            }
        }

        Poll::Ready(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use static_assertions::assert_impl_all;

    assert_impl_all!(Error<std::io::Error, std::io::Error>: std::error::Error, Send, Sync, Into<BoxError>);

    #[cfg(feature = "mocks")]
    use crate::client::conn::transport::mock::MockTransport;
    use crate::BoxError;

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
        use crate::client::conn::transport::mock::MockExecutor;

        let key: Key = "http://localhost:8080".parse().unwrap();

        let transport = MockTransport::single();

        let checkout: Checkout<_, _, _, MockExecutor> = Checkout::detached(
            key,
            transport.connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
        );

        let dbg = format!("{:?}", checkout);
        assert!(
            dbg.starts_with("Checkout { key: Key(\"http\", Some(localhost:8080)), pool: PoolRef(WeakOpt(None)), waiter: NoPool, inner: Connecting")
        );

        let connection = checkout.await.unwrap();
        assert!(connection.is_open());
    }
}
