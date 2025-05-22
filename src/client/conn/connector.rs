//! Connectors couple a transport with a protocol to create a connection.
//!
//! In a high-level client, the connector is integrated with the connection pool, to facilitate
//! connection re-use and pre-emption. The connector here is instead meant to be used without
//! a connection pool, when it is known that a new connection should be created every time
//! that the service gets called.
//!
//! This can be useful if you are developing or testing a transport or protocol implementation.
//! Creating a `Connector` object and awaiting it will give you a connection to the server,
//! which will obey the `Connection` trait.

use std::fmt;
use std::future::Future;
use std::future::IntoFuture;
use std::pin::Pin;

use std::task::ready;
use std::task::Context;
use std::task::Poll;

use http_body::Body;
use pin_project::pin_project;
use thiserror::Error;

use crate::client::conn::protocol::HttpProtocol;
use crate::client::conn::Protocol;
use crate::client::conn::Transport;
use crate::info::ConnectionInfo;
use crate::info::HasConnectionInfo;

use crate::client::conn::connection::Connection;
use crate::client::conn::connection::ConnectionError;
use crate::client::Error as ClientError;
use crate::service::ExecuteRequest;
use crate::BoxError;

pub(in crate::client) struct ConnectorMeta {
    overall_span: tracing::Span,
    transport_span: Option<tracing::Span>,
    protocol_span: Option<tracing::Span>,
}

impl ConnectorMeta {
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
#[allow(clippy::large_enum_variant)]
enum ConnectorState<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
{
    PollReadyTransport {
        parts: Option<http::request::Parts>,
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
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectorState::PollReadyTransport { parts, .. } => f
                .debug_struct("PollReadyTransport")
                .field("address", &parts.as_ref().unwrap().uri)
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
{
    /// Create a new connection from a transport connector and a protocol.
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
                parts: Some(parts),
                transport: Some(transport),
                protocol: Some(protocol),
            },
            version: Some(version),
            shareable,
        }
    }
}

#[allow(type_alias_bounds)]
type ConnectorError<T: Transport, P: Protocol<T::IO, B>, B> =
    Error<<T as Transport>::Error, <P as Protocol<T::IO, B>>::Error>;

impl<T, P, B> Connector<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
{
    pub(in crate::client) fn poll_connector<F>(
        self: Pin<&mut Self>,
        notify: F,
        meta: &mut ConnectorMeta,
        cx: &mut Context<'_>,
    ) -> Poll<Result<P::Connection, ConnectorError<T, P, B>>>
    where
        F: FnOnce(),
    {
        let mut connector_projected = self.project();
        let mut notifier = Some(notify);

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
                    let future = transport.connect(
                        parts
                            .take()
                            .expect("future polled in invalid state: parts is None"),
                    );
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
                        if let Some(notifier) = notifier.take() {
                            notifier();
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
{
    #[pin]
    connector: Connector<T, P, B>,
    meta: ConnectorMeta,
}

impl<T, P, B> fmt::Debug for ConnectorFuture<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
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
{
    type Output = Result<P::Connection, ConnectorError<T, P, B>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.as_mut().project();

        this.connector.poll_connector(|| (), this.meta, cx)
    }
}

impl<T, P, B> IntoFuture for Connector<T, P, B>
where
    T: Transport,
    P: Protocol<T::IO, B>,
{
    type Output = Result<P::Connection, ConnectorError<T, P, B>>;
    type IntoFuture = ConnectorFuture<T, P, B>;

    fn into_future(self) -> Self::IntoFuture {
        let meta = ConnectorMeta::new();

        ConnectorFuture {
            connector: self,
            meta,
        }
    }
}

/// A layer which provides a connection for a request.
///
/// No pooling is done.
#[derive(Debug, Clone)]
pub struct ConnectorLayer<T, P> {
    transport: T,
    protocol: P,
}

impl<T, P> ConnectorLayer<T, P> {
    /// Create a new `ConnectorLayer` wrapping the given transport and protocol.
    pub fn new(transport: T, protocol: P) -> Self {
        Self {
            transport,
            protocol,
        }
    }
}

impl<S, T, P> tower::layer::Layer<S> for ConnectorLayer<T, P>
where
    T: Clone,
    P: Clone,
{
    type Service = ConnectorService<S, T, P>;

    fn layer(&self, inner: S) -> Self::Service {
        ConnectorService::new(inner, self.transport.clone(), self.protocol.clone())
    }
}

/// A service that opens a connection with a given transport and protocol.
#[derive(Debug, Clone)]
pub struct ConnectorService<S, T, P> {
    inner: S,
    transport: T,
    protocol: P,
}

impl<S, T, P> ConnectorService<S, T, P> {
    /// Create a new `ConnectorService` wrapping the given service, transport, and protocol.
    pub fn new(inner: S, transport: T, protocol: P) -> Self {
        Self {
            inner,
            transport,
            protocol,
        }
    }
}

impl<S, T, P, C, BIn, BOut> tower::Service<http::Request<BIn>> for ConnectorService<S, T, P>
where
    C: Connection<BIn, ResBody = BOut>,
    P: Protocol<T::IO, BIn, Connection = C, Error = ConnectionError>
        + Clone
        + Send
        + Sync
        + 'static,
    T: Transport + Clone + Send + 'static,
    T::IO: Unpin,
    <<T as Transport>::IO as HasConnectionInfo>::Addr: Send,
    S: tower::Service<ExecuteRequest<C, BIn>, Response = http::Response<BOut>>
        + Clone
        + Send
        + 'static,
    S::Error: Into<ClientError>,
    BOut: Body + Unpin + 'static,
    BIn: Body + Unpin + Send + 'static,
    <BIn as Body>::Data: Send,
    <BIn as Body>::Error: Into<BoxError>,
{
    type Response = http::Response<BOut>;
    type Error = ClientError;
    type Future = self::future::ResponseFuture<T, P, C, S, BIn, BOut>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.transport
            .poll_ready(cx)
            .map_err(|error| ClientError::Connection(error.into()))
    }

    fn call(&mut self, req: http::Request<BIn>) -> Self::Future {
        let (parts, body) = req.into_parts();

        let connector = Connector::new(
            self.transport.clone(),
            self.protocol.clone(),
            parts.clone(),
            parts.version.into(),
        );

        let req = http::Request::from_parts(parts, body);

        self::future::ResponseFuture::new(connector, req, self.inner.clone())
    }
}

mod future {
    use super::{Connector, ConnectorMeta};

    use std::fmt;
    use std::future::Future;
    use std::task::Poll;

    use http_body::Body;
    use pin_project::pin_project;

    use crate::client::conn::connection::ConnectionError;
    use crate::client::conn::{Connection, Protocol, Transport};
    use crate::client::Error as ClientError;
    use crate::service::ExecuteRequest;
    use crate::BoxError;

    /// A future that resolves to an HTTP response.
    #[pin_project]
    pub struct ResponseFuture<T, P, C, S, BIn, BOut>
    where
        T: Transport + Send + 'static,
        P: Protocol<T::IO, BIn, Connection = C> + Send + 'static,
        C: Connection<BIn>,
        S: tower::Service<ExecuteRequest<C, BIn>, Response = http::Response<BOut>> + Send + 'static,
        BIn: 'static,
    {
        #[pin]
        inner: ResponseFutureState<T, P, C, S, BIn, BOut>,
        meta: ConnectorMeta,

        _body: std::marker::PhantomData<fn(BIn) -> BOut>,
    }

    impl<T, P, C, S, BIn, BOut> fmt::Debug for ResponseFuture<T, P, C, S, BIn, BOut>
    where
        T: Transport + Send + 'static,
        P: Protocol<T::IO, BIn, Connection = C> + Send + 'static,
        C: Connection<BIn>,
        S: tower::Service<ExecuteRequest<C, BIn>, Response = http::Response<BOut>> + Send + 'static,
        BIn: 'static,
    {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("ResponseFuture").finish()
        }
    }

    impl<T, P, C, S, BIn, BOut> ResponseFuture<T, P, C, S, BIn, BOut>
    where
        T: Transport + Send + 'static,
        P: Protocol<T::IO, BIn, Connection = C> + Send + 'static,
        C: Connection<BIn>,
        S: tower::Service<ExecuteRequest<C, BIn>, Response = http::Response<BOut>> + Send + 'static,
        BIn: 'static,
    {
        pub(super) fn new(
            connector: Connector<T, P, BIn>,
            request: http::Request<BIn>,
            service: S,
        ) -> Self {
            Self {
                inner: ResponseFutureState::Connect {
                    connector,
                    request: Some(request),
                    service,
                },
                meta: ConnectorMeta::new(),
                _body: std::marker::PhantomData,
            }
        }

        #[allow(dead_code)]
        fn error(error: ConnectionError) -> Self {
            Self {
                inner: ResponseFutureState::ConnectionError(Some(error)),
                meta: ConnectorMeta::new(),
                _body: std::marker::PhantomData,
            }
        }
    }

    impl<T, P, C, S, BIn, BOut> Future for ResponseFuture<T, P, C, S, BIn, BOut>
    where
        T: Transport + Send + 'static,
        <T as Transport>::Error: Into<BoxError>,
        P: Protocol<T::IO, BIn, Connection = C> + Send + 'static,
        <P as Protocol<T::IO, BIn>>::Error: Into<BoxError>,
        C: Connection<BIn>,
        S: tower::Service<ExecuteRequest<C, BIn>, Response = http::Response<BOut>> + Send + 'static,
        S::Error: Into<ClientError>,
        BOut: Body + Unpin + 'static,
        BIn: Body + Unpin + Send + 'static,
        <BIn as Body>::Data: Send,
        <BIn as Body>::Error: Into<BoxError>,
    {
        type Output = Result<http::Response<BOut>, ClientError>;

        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> Poll<Self::Output> {
            loop {
                let mut this = self.as_mut().project();
                let next = match this.inner.as_mut().project() {
                    ResponseFutureStateProj::Connect {
                        connector,
                        request,
                        service,
                    } => match connector.poll_connector(|| (), this.meta, cx) {
                        Poll::Ready(Ok(conn)) => {
                            ResponseFutureState::Request(service.call(ExecuteRequest::new(
                                conn,
                                request.take().expect("request polled again"),
                            )))
                        }
                        Poll::Ready(Err(error)) => {
                            return Poll::Ready(Err(error.into()));
                        }
                        Poll::Pending => {
                            return Poll::Pending;
                        }
                    },
                    ResponseFutureStateProj::Request(fut) => match fut.poll(cx) {
                        Poll::Ready(Ok(response)) => {
                            return Poll::Ready(Ok(response));
                        }
                        Poll::Ready(Err(error)) => {
                            return Poll::Ready(Err(error.into()));
                        }
                        Poll::Pending => {
                            return Poll::Pending;
                        }
                    },
                    ResponseFutureStateProj::ConnectionError(error) => {
                        return Poll::Ready(Err(ClientError::Connection(
                            error.take().expect("error polled again").into(),
                        )));
                    }
                };
                this.inner.set(next);
            }
        }
    }

    #[pin_project(project=ResponseFutureStateProj)]
    #[allow(clippy::large_enum_variant)]
    enum ResponseFutureState<T, P, C, S, BIn, BOut>
    where
        T: Transport + Send + 'static,
        P: Protocol<T::IO, BIn, Connection = C> + Send + 'static,
        C: Connection<BIn>,
        S: tower::Service<ExecuteRequest<C, BIn>, Response = http::Response<BOut>> + Send + 'static,
        BIn: 'static,
    {
        Connect {
            #[pin]
            connector: Connector<T, P, BIn>,
            request: Option<http::Request<BIn>>,
            service: S,
        },
        ConnectionError(Option<ConnectionError>),
        Request(#[pin] S::Future),
    }
}
