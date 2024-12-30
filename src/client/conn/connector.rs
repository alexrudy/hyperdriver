//! Connectors couple a transport with a protocol to create a connection.

use std::fmt;
use std::future::Future;
use std::future::IntoFuture;
use std::pin::Pin;

use std::task::ready;
use std::task::Context;
use std::task::Poll;

use pin_project::pin_project;
use thiserror::Error;
use tracing::trace;

use crate::client::conn::protocol::HttpProtocol;
use crate::client::conn::Protocol;
use crate::client::conn::Transport;
use crate::info::ConnectionInfo;
use crate::info::HasConnectionInfo;

use crate::client::pool::PoolRef;
use crate::client::pool::PoolableConnection;
use crate::client::pool::Token;

pub(in crate::client) struct CheckoutMeta {
    overall_span: tracing::Span,
    transport_span: Option<tracing::Span>,
    protocol_span: Option<tracing::Span>,
}

impl CheckoutMeta {
    pub(in crate::client) fn new() -> Self {
        let overall_span = tracing::Span::current();

        Self {
            overall_span,
            transport_span: None,
            protocol_span: None,
        }
    }

    pub(in crate::client) fn current(&self) -> &tracing::Span {
        &self.overall_span
    }

    pub(in crate::client) fn transport(&mut self) -> &tracing::Span {
        self.transport_span
            .get_or_insert_with(|| tracing::trace_span!(parent: &self.overall_span, "transport"))
    }

    pub(in crate::client) fn protocol(&mut self) -> &tracing::Span {
        self.protocol_span
            .get_or_insert_with(|| tracing::trace_span!(parent: &self.overall_span, "protocol"))
    }
}

/// Error that can occur during the connection process.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error<Transport, Protocol> {
    /// Error occurred during the connection
    #[error("creating connection")]
    Connecting(#[source] Transport),

    /// Error occurred during the handshake
    #[error("handshaking connection")]
    Handshaking(#[source] Protocol),

    /// Connection can't even be attempted
    #[error("connection closed")]
    Unavailable,
}

#[pin_project(project = ConnectorStateProjected)]
enum ConnectorState<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
    P::Connection: PoolableConnection,
{
    PollReadyTransport {
        parts: http::request::Parts,
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
            ConnectorState::PollReadyTransport { parts, .. } => f
                .debug_struct("PollReadyTransport")
                .field("address", &parts.uri)
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
    pub fn new(
        transport: T,
        protocol: P,
        parts: http::request::Parts,
        version: HttpProtocol,
    ) -> Self {
        //TODO: Fix this
        let shareable = false;

        Self {
            state: ConnectorState::PollReadyTransport {
                parts,
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
    pub(in crate::client) fn poll_connector(
        self: Pin<&mut Self>,
        pool: &PoolRef<P::Connection>,
        token: Token,
        meta: &mut CheckoutMeta,
        cx: &mut Context<'_>,
    ) -> Poll<Result<P::Connection, ConnectionError<T, P, B>>> {
        let mut connector_projected = self.project();

        loop {
            match connector_projected.state.as_mut().project() {
                ConnectorStateProjected::PollReadyTransport {
                    parts,
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
                    let future = transport.connect(parts.clone());
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
                            ?token,
                            "connection can be shared, telling pool to wait for handshake"
                        );
                        if let Some(mut pool) = pool.lock() {
                            pool.connected_in_handshake(token);
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

/// A future that resolves to a connection.
#[pin_project]
pub struct ConnectorFuture<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
    P::Connection: PoolableConnection,
{
    #[pin]
    connector: Connector<T, P, B>,
    pool: PoolRef<P::Connection>,
    token: Token,
    meta: CheckoutMeta,
}

impl<T, P, B> fmt::Debug for ConnectorFuture<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
    P::Connection: PoolableConnection,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ConnectorFuture")
            .field(&self.connector)
            .finish()
    }
}

impl<T, P, B> Future for ConnectorFuture<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
    P::Connection: PoolableConnection,
{
    type Output = Result<P::Connection, ConnectionError<T, P, B>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.as_mut().project();

        this.connector
            .poll_connector(&*this.pool, *this.token, this.meta, cx)
    }
}

impl<T, P, B> IntoFuture for Connector<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
    P::Connection: PoolableConnection,
{
    type Output = Result<P::Connection, ConnectionError<T, P, B>>;
    type IntoFuture = ConnectorFuture<T, P, B>;

    fn into_future(self) -> Self::IntoFuture {
        let pool = PoolRef::none();
        let token = Token::zero();
        let meta = CheckoutMeta::new();

        ConnectorFuture {
            connector: self,
            pool,
            token,
            meta,
        }
    }
}
