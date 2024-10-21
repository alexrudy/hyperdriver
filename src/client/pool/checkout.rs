use std::fmt;
use std::future::Future;
use std::pin::Pin;

use std::task::ready;
use std::task::Context;
use std::task::Poll;

use http::Uri;
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
pub(crate) enum Waiting<C: PoolableConnection, K: Key> {
    /// The checkout is waiting on an idle connection, and should
    /// attempt its own connection in the interim as well.
    Idle(#[pin] Receiver<Pooled<C, K>>),

    /// The checkout is waiting on a connection currently in the process
    /// of connecting, and should wait for that connection to complete,
    /// not starting its own connection.
    Connecting(#[pin] Receiver<Pooled<C, K>>),

    /// There is no pool for connections to wait for.
    NoPool,
}

impl<C: PoolableConnection, K: Key> Waiting<C, K> {
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

impl<C: PoolableConnection, K: Key> fmt::Debug for Waiting<C, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Waiting::Idle(_) => f.debug_tuple("Idle").finish(),
            Waiting::Connecting(_) => f.debug_tuple("Connecting").finish(),
            Waiting::NoPool => f.debug_tuple("NoPool").finish(),
        }
    }
}

pub(crate) enum WaitingPoll<C: PoolableConnection, K: Key> {
    Connected(Pooled<C, K>),
    Closed,
    NotReady,
}

impl<C: PoolableConnection, K: Key> Future for Waiting<C, K> {
    type Output = WaitingPoll<C, K>;

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
    fn poll_connector<K: Key>(
        self: Pin<&mut Self>,
        pool: &PoolRef<P::Connection, K>,
        key: &K,
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
                        trace!(
                            ?key,
                            "connection can be shared, telling pool to wait for handshake"
                        );
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

#[pin_project(project = CheckoutConnectingProj)]
pub(crate) enum InnerCheckoutConnecting<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
    P::Connection: PoolableConnection,
{
    Waiting,
    Connected,
    Connecting(#[pin] Connector<T, P, B>),
    ConnectingWithDelayDrop(Option<Pin<Box<Connector<T, P, B>>>>),
    ConnectingDelayed(Pin<Box<Connector<T, P, B>>>),
}

impl<T, P, B> fmt::Debug for InnerCheckoutConnecting<T, P, B>
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
            InnerCheckoutConnecting::ConnectingWithDelayDrop(connector) => f
                .debug_tuple("ConnectingWithDelayDrop")
                .field(connector)
                .finish(),
            InnerCheckoutConnecting::ConnectingDelayed(connector) => {
                f.debug_tuple("ConnectingDelayed").field(connector).finish()
            }
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
pub(crate) struct Checkout<T, P, B, K>
where
    T: Transport + 'static,
    P: Protocol<T::IO, B> + Send + 'static,
    P::Connection: PoolableConnection,
    B: 'static,
    K: Key,
{
    key: K,
    pool: PoolRef<P::Connection, K>,
    #[pin]
    waiter: Waiting<P::Connection, K>,
    #[pin]
    inner: InnerCheckoutConnecting<T, P, B>,
    connection: Option<P::Connection>,
    meta: CheckoutMeta,
    #[cfg(debug_assertions)]
    id: CheckoutId,
}

impl<T, P, B, K> fmt::Debug for Checkout<T, P, B, K>
where
    T: Transport + Send + 'static,
    P: Protocol<T::IO, B> + Send + 'static,
    P::Connection: PoolableConnection,
    B: 'static,
    K: Key,
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

impl<T, P, B, K, C> Checkout<T, P, B, K>
where
    T: Transport + 'static,
    P: Protocol<T::IO, B, Connection = C> + Send + 'static,
    P::Connection: PoolableConnection,
    B: 'static,
    K: Key,
{
    /// Converts this checkout into a "delayed drop" checkout.
    fn as_delayed(self: Pin<&mut Self>) -> Option<Self> {
        let mut this = self.project();

        match this.inner.as_mut().project() {
            CheckoutConnectingProj::ConnectingWithDelayDrop(connector) if connector.is_some() => {
                tracing::trace!("converting checkout to delayed drop");
                Some(Checkout {
                    key: this.key.clone(),
                    pool: this.pool.clone(),
                    waiter: Waiting::NoPool,
                    inner: InnerCheckoutConnecting::ConnectingDelayed(connector.take().unwrap()),
                    connection: None,
                    meta: CheckoutMeta::new(), // New meta to avoid holding spans in the spawned task
                    #[cfg(debug_assertions)]
                    id: *this.id,
                })
            }
            _ => None,
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
    pub(crate) fn detached(key: K, connector: Connector<T, P, B>) -> Self {
        #[cfg(debug_assertions)]
        let id = CheckoutId::new();

        #[cfg(debug_assertions)]
        tracing::trace!(?key, %id, "creating detached checkout");

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

    pub(super) fn new(
        key: K,
        pool: &Pool<P::Connection, K>,
        waiter: Receiver<Pooled<P::Connection, K>>,
        connect: Option<Connector<T, P, B>>,
        connection: Option<P::Connection>,
        config: &Config,
    ) -> Self {
        #[cfg(debug_assertions)]
        let id = CheckoutId::new();
        let meta = CheckoutMeta::new();

        #[cfg(debug_assertions)]
        tracing::trace!(?key, %id, "creating new checkout");

        if connection.is_some() {
            tracing::trace!(?key, "connection recieved from pool");
            Self {
                key,
                pool: pool.as_ref(),
                waiter: Waiting::Idle(waiter),
                inner: InnerCheckoutConnecting::Connected,
                connection,
                meta,
                #[cfg(debug_assertions)]
                id,
            }
        } else if let Some(connector) = connect {
            tracing::trace!(?key, "connecting to pool");

            let inner = if config.continue_after_preemption {
                InnerCheckoutConnecting::ConnectingWithDelayDrop(Some(Box::pin(connector)))
            } else {
                InnerCheckoutConnecting::Connecting(connector)
            };

            Self {
                key,
                pool: pool.as_ref(),
                waiter: Waiting::Idle(waiter),
                inner,
                connection,
                meta,
                #[cfg(debug_assertions)]
                id,
            }
        } else {
            tracing::trace!(?key, "waiting for connection");
            Self {
                key,
                pool: pool.as_ref(),
                waiter: Waiting::Connecting(waiter),
                inner: InnerCheckoutConnecting::Waiting,
                connection,
                meta,
                #[cfg(debug_assertions)]
                id,
            }
        }
    }
}

impl<T, P, B, K> Future for Checkout<T, P, B, K>
where
    T: Transport + 'static,
    P: Protocol<T::IO, B> + Send + 'static,
    P::Connection: PoolableConnection,
    B: 'static,
    K: Key,
{
    type Output = Result<
        Pooled<P::Connection, K>,
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
                debug!(key=?this.key, "connection recieved from waiter");

                return Poll::Ready(Ok(connection));
            }
        }

        trace!(key=?this.key, "polling for new connection");
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
            CheckoutConnectingProj::ConnectingWithDelayDrop(Some(connector))
            | CheckoutConnectingProj::ConnectingDelayed(connector) => {
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
            CheckoutConnectingProj::ConnectingWithDelayDrop(None) => {
                // Something stole our connection, this is an error state.
                panic!("connection was stolen from checkout")
            }
        }
    }
}

/// Register a connection with the pool referenced here.
fn register_connected<C, K>(poolref: &PoolRef<C, K>, key: &K, mut connection: C) -> Pooled<C, K>
where
    C: PoolableConnection,
    K: Key,
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
impl<T, P, B, K> PinnedDrop for Checkout<T, P, B, K>
where
    T: Transport + 'static,
    P: Protocol<T::IO, B> + Send + 'static,
    P::Connection: PoolableConnection,
    B: 'static,
    K: Key,
{
    fn drop(mut self: Pin<&mut Self>) {
        #[cfg(debug_assertions)]
        tracing::trace!(id=%self.id, "drop for checkout");

        if let Some(checkout) = self.as_mut().as_delayed() {
            tokio::task::spawn(async move {
                if let Err(err) = checkout.await {
                    tracing::error!(error=%err, "error during delayed drop");
                }
            });
        } else if let Some(mut pool) = self.pool.lock() {
            // Connection is only cancled when no delayed drop occurs.
            pool.cancel_connection(&self.key);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[cfg(feature = "mocks")]
    use crate::client::pool::UriKey;

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
        let key: UriKey = "http://localhost:8080".parse().unwrap();

        let transport = MockTransport::single();

        let checkout = Checkout::detached(
            key.clone(),
            transport.connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
        );

        assert_eq!(checkout.key, key);
        assert!(checkout.pool.is_none());
        assert!(matches!(
            checkout.inner,
            InnerCheckoutConnecting::Connecting(_)
        ));
        assert!(matches!(checkout.waiter, Waiting::NoPool));

        let dbg = format!("{:?}", checkout);
        assert!(dbg.starts_with("Checkout { "));

        let connection = checkout.await.unwrap();
        assert!(connection.is_open());
    }
}
