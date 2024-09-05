use std::fmt;
use std::future::Future;
use std::task::Poll;

use http_body::Body;
use pin_project::pin_project;
use tower::util::Oneshot;
use tower::ServiceExt;

use crate::client::conn::connection::ConnectionError;
use crate::client::conn::connection::HttpConnection;
use crate::client::conn::protocol::auto::HttpConnectionBuilder;
use crate::client::conn::protocol::HttpProtocol;
use crate::client::conn::transport::tcp::TcpTransport;
use crate::client::conn::Connection;
use crate::client::conn::Protocol;
use crate::client::conn::TlsTransport;
use crate::client::conn::Transport;
use crate::client::pool;
use crate::client::pool::Checkout;
use crate::client::pool::Connector;
use crate::client::pool::PoolableConnection;
use crate::client::Error;
use crate::info::HasConnectionInfo;
use crate::service::client::ExecuteRequest;
use crate::BoxError;

use super::PoolableStream;

/// Layer which adds connection pooling and converts
/// to an inner service which accepts `ExecuteRequest`
/// from an outer service which accepts `http::Request`.
pub struct ConnectionPoolLayer<T, P, BIn> {
    transport: T,
    protocol: P,
    pool: Option<pool::Config>,
    _body: std::marker::PhantomData<fn(BIn) -> ()>,
}

impl<T: fmt::Debug, P: fmt::Debug, BIn> fmt::Debug for ConnectionPoolLayer<T, P, BIn> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionPoolLayer")
            .field("transport", &self.transport)
            .field("protocol", &self.protocol)
            .field("pool", &self.pool)
            .finish()
    }
}

impl<T, P, BIn> ConnectionPoolLayer<T, P, BIn> {
    /// Layer for connection pooling.
    pub fn new(transport: T, protocol: P) -> Self {
        Self {
            transport,
            protocol,
            pool: None,
            _body: std::marker::PhantomData,
        }
    }

    /// Set the connection pool configuration.
    pub fn with_pool(mut self, pool: pool::Config) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Set the connection pool configuration to an optional value.
    pub fn with_optional_pool(mut self, pool: Option<pool::Config>) -> Self {
        self.pool = pool;
        self
    }

    /// Disable connection pooling.
    pub fn without_pool(mut self) -> Self {
        self.pool = None;
        self
    }
}

impl<T, P, BIn> Clone for ConnectionPoolLayer<T, P, BIn>
where
    T: Clone,
    P: Clone,
{
    fn clone(&self) -> Self {
        Self {
            transport: self.transport.clone(),
            protocol: self.protocol.clone(),
            pool: self.pool.clone(),
            _body: std::marker::PhantomData,
        }
    }
}

impl<T, P, S, BIn> tower::layer::Layer<S> for ConnectionPoolLayer<T, P, BIn>
where
    T: Transport + Clone + Send + Sync + 'static,
    P: Protocol<T::IO, BIn> + Clone + Send + Sync + 'static,
    P::Connection: PoolableConnection,
{
    type Service = ConnectionPoolService<T, P, S, BIn>;

    fn layer(&self, service: S) -> Self::Service {
        let pool = self.pool.clone().map(pool::Pool::new);

        ConnectionPoolService {
            transport: self.transport.clone(),
            protocol: self.protocol.clone(),
            service,
            pool,
            _body: std::marker::PhantomData,
        }
    }
}

/// A service which gets a connection from a possible connection pool and passes it to
/// an inner service to execute that request.
///
/// This service will accept [`http::Request`] objects, but expects the inner service
/// to accept [`ExecuteRequest`] objects, which bundle the connection with the request.
///
/// The simplest interior service is [`crate::service::RequestExecutor`], which will execute the request
/// on the connection and return the response.
#[derive(Debug)]
pub struct ConnectionPoolService<T, P, S, BIn = crate::Body>
where
    T: Transport,
    P: Protocol<T::IO, BIn>,
    P::Connection: PoolableConnection,
{
    pub(super) transport: T,
    pub(super) protocol: P,
    pub(super) service: S,
    pub(super) pool: Option<pool::Pool<P::Connection>>,
    pub(super) _body: std::marker::PhantomData<fn(BIn) -> ()>,
}

impl<T, P, S, BIn> ConnectionPoolService<T, P, S, BIn>
where
    T: Transport,
    P: Protocol<T::IO, BIn>,
    P::Connection: PoolableConnection,
{
    /// Create a new client with the given transport, protocol, and pool configuration.
    pub fn new(transport: T, protocol: P, service: S, pool: pool::Config) -> Self {
        Self {
            transport,
            protocol,
            service,
            pool: Some(pool::Pool::new(pool)),
            _body: std::marker::PhantomData,
        }
    }

    /// Disable connection pooling for this client.
    pub fn without_pool(self) -> Self {
        Self { pool: None, ..self }
    }
}

impl
    ConnectionPoolService<
        TlsTransport<TcpTransport>,
        HttpConnectionBuilder<crate::Body>,
        crate::service::client::RequestExecutor<HttpConnection<crate::Body>, crate::Body>,
        crate::Body,
    >
{
    /// Create a new client with the default configuration for making requests over TCP
    /// connections using the HTTP protocol.
    ///
    /// When the `tls` feature is enabled, this will also add support for `tls` when
    /// using the `https` scheme, with a default TLS configuration that will rely
    /// on the system's certificate store.
    pub fn new_tcp_http() -> Self {
        Self {
            pool: Some(pool::Pool::new(pool::Config {
                idle_timeout: Some(std::time::Duration::from_secs(90)),
                max_idle_per_host: 32,
                continue_after_preemption: true,
            })),

            transport: Default::default(),

            protocol: HttpConnectionBuilder::default(),

            service: crate::service::client::RequestExecutor::new(),

            _body: std::marker::PhantomData,
        }
    }
}

impl<P, T, S, BIn> Clone for ConnectionPoolService<T, P, S, BIn>
where
    P: Protocol<T::IO, BIn> + Clone,
    P::Connection: PoolableConnection,
    T: Transport + Clone,
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            protocol: self.protocol.clone(),
            transport: self.transport.clone(),
            pool: self.pool.clone(),
            service: self.service.clone(),
            _body: std::marker::PhantomData,
        }
    }
}

impl<T, P, S, BIn> ConnectionPoolService<T, P, S, BIn>
where
    T: Transport,
    T::IO: Unpin,
    P: Protocol<T::IO, BIn, Error = ConnectionError> + Clone + Send + Sync + 'static,
    <P as Protocol<T::IO, BIn>>::Connection: PoolableConnection + Send + 'static,
    <<T as Transport>::IO as HasConnectionInfo>::Addr: Send,
{
    #[allow(clippy::type_complexity)]
    fn connect_to(
        &self,
        uri: http::Uri,
        http_protocol: HttpProtocol,
    ) -> Result<Checkout<T, P, BIn>, ConnectionError> {
        let key: pool::Key = uri.clone().try_into()?;
        let protocol = self.protocol.clone();
        let transport = self.transport.clone();

        let connector = Connector::new(transport, protocol, uri, http_protocol);

        if let Some(pool) = self.pool.as_ref() {
            tracing::trace!(%key, "checking out connection");
            Ok(pool.checkout(key, http_protocol.multiplex(), connector))
        } else {
            tracing::trace!(%key, "detatched connection");
            Ok(Checkout::detached(key, connector))
        }
    }
}

impl<P, C, T, S, BIn, BOut> tower::Service<http::Request<BIn>>
    for ConnectionPoolService<T, P, S, BIn>
where
    C: Connection<BIn, ResBody = BOut> + PoolableConnection,
    P: Protocol<T::IO, BIn, Connection = C, Error = ConnectionError>
        + Clone
        + Send
        + Sync
        + 'static,
    T: Transport + Send + 'static,
    T::IO: PoolableStream + Unpin,
    <<T as Transport>::IO as HasConnectionInfo>::Addr: Send,
    S: tower::Service<ExecuteRequest<C, BIn>, Response = http::Response<BOut>>
        + Clone
        + Send
        + 'static,
    S::Error: Into<Error>,
    BOut: Body + Unpin + 'static,
    BIn: Body + Unpin + Send + 'static,
    <BIn as Body>::Data: Send,
    <BIn as Body>::Error: Into<BoxError>,
{
    type Response = http::Response<BOut>;
    type Error = Error;
    type Future = ResponseFuture<T, P, C, S, BIn, BOut>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<BIn>) -> Self::Future {
        let uri = request.uri().clone();

        let protocol: HttpProtocol = request.version().into();

        match self.connect_to(uri, protocol) {
            Ok(checkout) => ResponseFuture::new(checkout, request, self.service.clone()),
            Err(error) => ResponseFuture::error(error),
        }
    }
}

impl<P, C, T, S, BIn, BOut> ConnectionPoolService<T, P, S, BIn>
where
    C: Connection<BIn, ResBody = BOut> + PoolableConnection,
    P: Protocol<T::IO, BIn, Connection = C, Error = ConnectionError>
        + Clone
        + Send
        + Sync
        + 'static,
    T: Transport + 'static,
    T::IO: PoolableStream + Unpin,
    S: tower::Service<ExecuteRequest<C, BIn>, Response = http::Response<BOut>>
        + Clone
        + Send
        + 'static,
    S::Error: Into<Error>,
    BIn: Body + Unpin + Send + 'static,
    <BIn as Body>::Data: Send,
    <BIn as Body>::Error: Into<BoxError>,
    BOut: Body + Unpin + 'static,
    <<T as Transport>::IO as HasConnectionInfo>::Addr: Send,
{
    /// Send an http Request, and return a Future of the Response.
    pub fn request(&self, request: http::Request<BIn>) -> Oneshot<Self, http::Request<BIn>> {
        self.clone().oneshot(request)
    }
}

/// A future that resolves to an HTTP response.
#[pin_project]
pub struct ResponseFuture<T, P, C, S, BIn, BOut>
where
    T: Transport + Send + 'static,
    P: Protocol<T::IO, BIn, Connection = C> + Send + 'static,
    C: Connection<BIn> + PoolableConnection,
    S: tower::Service<ExecuteRequest<C, BIn>, Response = http::Response<BOut>> + Send + 'static,
    BIn: 'static,
{
    #[pin]
    inner: ResponseFutureState<T, P, C, S, BIn, BOut>,
    _body: std::marker::PhantomData<fn(BIn) -> BOut>,
}

impl<T, P, C, S, BIn, BOut> fmt::Debug for ResponseFuture<T, P, C, S, BIn, BOut>
where
    T: Transport + Send + 'static,
    P: Protocol<T::IO, BIn, Connection = C> + Send + 'static,
    C: Connection<BIn> + PoolableConnection,
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
    C: Connection<BIn> + PoolableConnection,
    S: tower::Service<ExecuteRequest<C, BIn>, Response = http::Response<BOut>> + Send + 'static,
    BIn: 'static,
{
    fn new(checkout: Checkout<T, P, BIn>, request: http::Request<BIn>, service: S) -> Self {
        Self {
            inner: ResponseFutureState::Checkout {
                checkout,
                request: Some(request),
                service,
            },
            _body: std::marker::PhantomData,
        }
    }

    fn error(error: ConnectionError) -> Self {
        Self {
            inner: ResponseFutureState::ConnectionError(Some(error)),
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
    C: Connection<BIn> + PoolableConnection,
    S: tower::Service<ExecuteRequest<C, BIn>, Response = http::Response<BOut>> + Send + 'static,
    S::Error: Into<crate::client::Error>,
    BOut: Body + Unpin + 'static,
    BIn: Body + Unpin + Send + 'static,
    <BIn as Body>::Data: Send,
    <BIn as Body>::Error: Into<BoxError>,
{
    type Output = Result<http::Response<BOut>, Error>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        loop {
            let mut this = self.as_mut().project();
            let next = match this.inner.as_mut().project() {
                ResponseFutureStateProj::Checkout {
                    checkout,
                    request,
                    service,
                } => match checkout.poll(cx) {
                    Poll::Ready(Ok(conn)) => ResponseFutureState::Request(service.call(
                        ExecuteRequest::new(conn, request.take().expect("request polled again")),
                    )),
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
                    return Poll::Ready(Err(Error::Connection(
                        error.take().expect("error polled again").into(),
                    )));
                }
            };
            this.inner.set(next);
        }
    }
}

#[pin_project(project=ResponseFutureStateProj)]
enum ResponseFutureState<T, P, C, S, BIn, BOut>
where
    T: Transport + Send + 'static,
    P: Protocol<T::IO, BIn, Connection = C> + Send + 'static,
    C: Connection<BIn> + PoolableConnection,
    S: tower::Service<ExecuteRequest<C, BIn>, Response = http::Response<BOut>> + Send + 'static,
    BIn: 'static,
{
    Checkout {
        #[pin]
        checkout: Checkout<T, P, BIn>,
        request: Option<http::Request<BIn>>,
        service: S,
    },
    ConnectionError(Option<ConnectionError>),
    Request(#[pin] S::Future),
}
