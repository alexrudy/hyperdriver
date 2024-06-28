//! HTTP client library for Rust, built on top of [hyper].

use std::fmt;
use std::future::poll_fn;
use std::future::Future;
use std::task::Poll;

use self::conn::Protocol;
use self::conn::Transport;
use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use http::uri::Port;
use http::uri::Scheme;
use http::HeaderValue;
use http::Uri;
use http::Version;
use thiserror::Error;
use tower::util::Oneshot;
use tower::ServiceExt;
use tracing::warn;

use crate::client::conn::connection::ConnectionError;
use crate::client::conn::protocol::auto::HttpConnectionBuilder;
use crate::client::conn::protocol::HttpProtocol;
use crate::client::conn::transport::tcp::TcpTransport;
use crate::client::conn::transport::TransportStream;
use crate::client::conn::Connection;
use crate::client::conn::TlsTransport;
use crate::client::pool::Checkout;
use crate::client::pool::Connector;
use crate::client::pool::{PoolableConnection, Pooled};
use crate::client::Error as HyperdriverError;
use crate::info::HasConnectionInfo;

mod builder;

pub mod conn;
pub mod pool;

pub use builder::Builder;

pub use pool::Config as PoolConfig;

/// Client error type.
#[derive(Debug, Error)]
pub enum Error {
    /// Error occured with the underlying connection.
    #[error(transparent)]
    Connection(Box<dyn std::error::Error + Send + Sync + 'static>),

    /// Error occured with the underlying transport.
    #[error("transport: {0}")]
    Transport(Box<dyn std::error::Error + Send + Sync + 'static>),

    /// Error occured with the underlying protocol.
    #[error("protocol: {0}")]
    Protocol(Box<dyn std::error::Error + Send + Sync + 'static>),

    /// Error occured with the user's request, such as an invalid URI.
    #[error("user error: {0}")]
    User(hyper::Error),

    /// Invalid HTTP Method for the current action.
    #[error("invalid method: {0}")]
    InvalidMethod(http::Method),

    /// Protocol is not supported by this client or transport.
    #[error("unsupported protocol")]
    UnsupportedProtocol,
}

impl From<pool::Error<ConnectionError>> for Error {
    fn from(error: pool::Error<ConnectionError>) -> Self {
        match error {
            pool::Error::Connecting(error) => Error::Connection(error.into()),
            pool::Error::Handshaking(error) => Error::Transport(error.into()),
            pool::Error::Unavailable => {
                Error::Connection("pool closed, no connection can be made".into())
            }
        }
    }
}

#[cfg(feature = "tls")]
/// Get a default TLS client configuration by loading the platform's native certificates.
pub fn default_tls_config() -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().expect("could not load platform certs") {
        roots.add(cert).unwrap();
    }

    let mut cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    cfg.alpn_protocols.push(b"h2".to_vec());
    cfg.alpn_protocols.push(b"http/1.1".to_vec());
    cfg
}

/// An inner client HTTP service.
#[derive(Debug)]
pub struct ClientService<T, P, BOut>
where
    T: Transport,
    P: Protocol<T::IO>,
    P::Connection: PoolableConnection,
{
    transport: T,
    protocol: P,
    pool: Option<pool::Pool<P::Connection>>,
    _body: std::marker::PhantomData<fn() -> BOut>,
}

impl<P, T, BOut> ClientService<T, P, BOut>
where
    T: Transport,
    P: Protocol<T::IO>,
    P::Connection: PoolableConnection,
{
    /// Create a new client with the given transport, protocol, and pool configuration.
    pub fn new(transport: T, protocol: P, pool: pool::Config) -> Self {
        Self {
            transport,
            protocol,
            pool: Some(pool::Pool::new(pool)),
            _body: std::marker::PhantomData,
        }
    }
}

impl ClientService<TlsTransport<TcpTransport>, HttpConnectionBuilder, crate::Body> {
    /// Create a new client with the default configuration.
    pub fn new_tcp_http() -> Self {
        Self {
            pool: Some(pool::Pool::new(pool::Config {
                idle_timeout: Some(std::time::Duration::from_secs(90)),
                max_idle_per_host: 32,
            })),

            transport: Default::default(),

            protocol: HttpConnectionBuilder::default(),

            _body: std::marker::PhantomData,
        }
    }
}

impl<P, T, B> Clone for ClientService<T, P, B>
where
    P: Protocol<T::IO> + Clone,
    P::Connection: PoolableConnection,
    T: Transport + Clone,
{
    fn clone(&self) -> Self {
        Self {
            protocol: self.protocol.clone(),
            transport: self.transport.clone(),
            pool: self.pool.clone(),
            _body: std::marker::PhantomData,
        }
    }
}

impl<P, C, T, B> ClientService<T, P, B>
where
    C: Connection + PoolableConnection,
    P: Protocol<T::IO, Connection = C, Error = ConnectionError> + Clone + Send + Sync + 'static,
    T: Transport + 'static,
    T::IO: Unpin,
    <<T as Transport>::IO as HasConnectionInfo>::Addr: Send,
{
    #[allow(clippy::type_complexity)]
    fn connect_to(
        &self,
        uri: http::Uri,
        http_protocol: HttpProtocol,
    ) -> Result<Checkout<P::Connection, TransportStream<T::IO>, ConnectionError>, ConnectionError>
    {
        let key: pool::Key = uri.clone().try_into()?;
        let mut protocol = self.protocol.clone();
        let mut transport = self.transport.clone();

        let connector = Connector::new(
            move || async move {
                poll_fn(|cx| Transport::poll_ready(&mut transport, cx))
                    .await
                    .map_err(|error| ConnectionError::Connecting(error.into()))?;
                transport
                    .connect(uri)
                    .await
                    .map_err(|error| ConnectionError::Connecting(error.into()))
            },
            Box::new(move |transport| {
                Box::pin(async move {
                    poll_fn(|cx| Protocol::poll_ready(&mut protocol, cx))
                        .await
                        .map_err(|error| ConnectionError::Handshake(error.into()))?;
                    protocol
                        .connect(transport, http_protocol)
                        .await
                        .map_err(|error| ConnectionError::Handshake(error.into()))
                }) as _
            }),
        );

        if let Some(pool) = self.pool.as_ref() {
            Ok(pool.checkout(key, http_protocol.multiplex(), connector))
        } else {
            Ok(Checkout::detached(key, connector))
        }
    }
}

impl<P, C, T, BIn, BOut> tower::Service<http::Request<BIn>> for ClientService<T, P, BOut>
where
    C: Connection + PoolableConnection,
    P: Protocol<T::IO, Connection = C, Error = ConnectionError> + Clone + Send + Sync + 'static,
    T: Transport + 'static,
    T::IO: Unpin,
    BIn: Into<crate::body::Body>,
    BOut: From<crate::body::Body> + Unpin,
    <<T as Transport>::IO as HasConnectionInfo>::Addr: Send,
    C::ResBody: Into<crate::Body>,
{
    type Response = http::Response<BOut>;
    type Error = Error;
    type Future = ResponseFuture<P::Connection, TransportStream<T::IO>, BOut>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<BIn>) -> Self::Future {
        let uri = request.uri().clone();

        let protocol: HttpProtocol = request.version().into();

        match self.connect_to(uri, protocol) {
            Ok(checkout) => ResponseFuture::new(checkout, request.map(Into::into)),
            Err(error) => ResponseFuture::error(error),
        }
    }
}

/// Client service for TCP connections with TLS and HTTP.
pub type ClientTlsTcpService =
    ClientService<TlsTransport<TcpTransport>, HttpConnectionBuilder, crate::Body>;

/// A simple async HTTP client.
///
/// This client is built on top of the `tokio` runtime and the `hyper` HTTP library.
/// It combines a connection pool with a transport layer to provide a simple API for
/// sending HTTP requests.
///
/// # Example
/// ```no_run
/// # use hyperdriver::client::Client;
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let mut client = Client::new_tcp_http();
/// let response = client.get("http://example.com".parse().unwrap()).await.unwrap();
/// println!("Response: {:?}", response);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Client<S = ClientTlsTcpService> {
    service: S,
}

impl<S> fmt::Debug for Client<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").finish()
    }
}

impl<T, P, B> Client<ClientService<T, P, B>>
where
    T: Transport,
    P: Protocol<T::IO>,
    P::Connection: PoolableConnection,
{
    /// Create a new client with the given connector and pool configuration.
    pub fn new(protocol: P, transport: T, pool: pool::Config) -> Self {
        Self {
            service: ClientService::new(transport, protocol, pool),
        }
    }
}

impl Client<()> {
    /// Create a new client builder
    pub fn builder() -> self::Builder {
        self::Builder::default()
    }
}

#[cfg(feature = "stream")]
impl Default for Client<ClientTlsTcpService> {
    fn default() -> Self {
        Self {
            service: ClientService::new_tcp_http(),
        }
    }
}

#[cfg(feature = "stream")]
impl Client<ClientTlsTcpService> {
    /// Create a new client that uses TCP and HTTP.
    pub fn new_tcp_http() -> Self {
        Self::default()
    }
}

impl<S> Client<S> {
    /// Apply a layer to the client
    pub fn layer<L>(self, layer: L) -> Client<L::Service>
    where
        L: tower::Layer<S>,
    {
        Client {
            service: layer.layer(self.service),
        }
    }
}

impl<S> Client<S>
where
    S: tower::Service<http::Request<crate::Body>, Response = http::Response<crate::Body>> + Clone,
    Error: From<S::Error>,
{
    /// Send an http Request, and return a Future of the Response.
    pub fn request(
        &mut self,
        request: crate::body::Request,
    ) -> Oneshot<S, http::Request<crate::Body>> {
        self.service.clone().oneshot(request)
    }

    /// Make a GET request to the given URI.
    pub async fn get(&mut self, uri: http::Uri) -> Result<http::Response<crate::Body>, Error> {
        let request = http::Request::get(uri.clone())
            .body(crate::body::Body::empty())
            .unwrap();

        let response = self.request(request).await?;
        Ok(response)
    }
}

/// A future that resolves to an HTTP response.
pub struct ResponseFuture<C, T, BOut = crate::Body>
where
    C: pool::PoolableConnection,
    T: pool::PoolableTransport,
{
    inner: ResponseFutureState<C, T>,
    _body: std::marker::PhantomData<fn() -> BOut>,
}

impl<C: pool::PoolableConnection, T: pool::PoolableTransport, B> fmt::Debug
    for ResponseFuture<C, T, B>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponseFuture").finish()
    }
}

impl<C, T, BOut> ResponseFuture<C, T, BOut>
where
    C: pool::PoolableConnection,
    T: pool::PoolableTransport,
{
    fn new(checkout: Checkout<C, T, ConnectionError>, request: crate::body::Request) -> Self {
        Self {
            inner: ResponseFutureState::Checkout { checkout, request },
            _body: std::marker::PhantomData,
        }
    }

    fn error(error: ConnectionError) -> Self {
        Self {
            inner: ResponseFutureState::ConnectionError(error),
            _body: std::marker::PhantomData,
        }
    }
}

impl<C, T, BOut> Future for ResponseFuture<C, T, BOut>
where
    C: Connection + pool::PoolableConnection,
    C::ResBody: Into<crate::body::Body>,
    T: pool::PoolableTransport,
    BOut: From<crate::body::Body> + Unpin,
{
    type Output = Result<http::Response<BOut>, Error>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        loop {
            match std::mem::replace(&mut self.inner, ResponseFutureState::Empty) {
                ResponseFutureState::Checkout {
                    mut checkout,
                    request,
                } => match checkout.poll_unpin(cx) {
                    Poll::Ready(Ok(conn)) => {
                        self.inner =
                            ResponseFutureState::Request(execute_request(request, conn).boxed());
                    }
                    Poll::Ready(Err(error)) => {
                        return Poll::Ready(Err(error.into()));
                    }
                    Poll::Pending => {
                        self.inner = ResponseFutureState::Checkout { checkout, request };
                        return Poll::Pending;
                    }
                },
                ResponseFutureState::Request(mut fut) => match fut.poll_unpin(cx) {
                    Poll::Ready(Ok(response)) => return Poll::Ready(Ok(response.map(Into::into))),
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Pending => {
                        self.inner = ResponseFutureState::Request(fut);
                        return Poll::Pending;
                    }
                },
                ResponseFutureState::ConnectionError(error) => {
                    return Poll::Ready(Err(Error::Connection(error.into())));
                }
                ResponseFutureState::Empty => {
                    panic!("future polled after completion");
                }
            }
        }
    }
}

enum ResponseFutureState<C: pool::PoolableConnection, T: pool::PoolableTransport> {
    Empty,
    Checkout {
        checkout: Checkout<C, T, ConnectionError>,
        request: crate::body::Request,
    },
    ConnectionError(ConnectionError),
    Request(BoxFuture<'static, Result<http::Response<crate::body::Body>, HyperdriverError>>),
}

/// Prepare a request for sending over the connection.
fn prepare_request<C: Connection + PoolableConnection>(
    request: &mut http::Request<crate::body::Body>,
    conn: &Pooled<C>,
) -> Result<(), Error> {
    request
        .headers_mut()
        .entry(http::header::USER_AGENT)
        .or_insert_with(|| {
            HeaderValue::from_static(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ))
        });

    if conn.version() == Version::HTTP_11 {
        if request.version() == Version::HTTP_2 {
            warn!("refusing to send HTTP/2 request to HTTP/1.1 connection");
            return Err(Error::UnsupportedProtocol);
        }

        //TODO: Configure set host header?
        set_host_header(request);

        if request.method() == http::Method::CONNECT {
            authority_form(request.uri_mut());

            // If the URI is to HTTPS, and the connector claimed to be a proxy,
            // then it *should* have tunneled, and so we don't want to send
            // absolute-form in that case.
            if request.uri().scheme() == Some(&Scheme::HTTPS) {
                origin_form(request.uri_mut());
            }
        } else if request.uri().scheme().is_none() || request.uri().authority().is_none() {
            absolute_form(request.uri_mut());
        } else {
            origin_form(request.uri_mut());
        }
    } else if request.method() == http::Method::CONNECT {
        return Err(Error::InvalidMethod(http::Method::CONNECT));
    } else if conn.version() == Version::HTTP_2 {
        set_host_header(request);
    }
    Ok(())
}

async fn execute_request<C>(
    mut request: crate::body::Request,
    mut conn: Pooled<C>,
) -> Result<http::Response<crate::body::Body>, Error>
where
    C: Connection + PoolableConnection,
    C::ResBody: Into<crate::Body>,
{
    prepare_request(&mut request, &conn)?;

    tracing::trace!(request.uri=%request.uri(), conn.version=?conn.version(), req.version=?request.version(), "sending request");

    let response = conn
        .send_request(request)
        .await
        .map_err(|error| Error::Connection(error.into()))?;

    // Shared connections are already in the pool, no need to do this.
    if !conn.can_share() {
        // Only re-insert the connection when it is ready again. Spawn
        // a task to wait for the connection to become ready before dropping.
        tokio::spawn(async move {
            let _ = conn.when_ready().await.map_err(|_| ());
        });
    }

    Ok(response.map(|body| body.into()))
}

/// Convert the URI to authority-form, if it is not already.
///
/// This is the form of the URI with just the authority and a default
/// path and scheme. This is used in HTTP/1 CONNECT requests.
fn authority_form(uri: &mut Uri) {
    *uri = match uri.authority() {
        Some(auth) => {
            let mut parts = ::http::uri::Parts::default();
            parts.authority = Some(auth.clone());
            Uri::from_parts(parts).expect("authority is valid")
        }
        None => {
            unreachable!("authority_form with relative uri");
        }
    };
}

fn absolute_form(uri: &mut Uri) {
    debug_assert!(uri.scheme().is_some(), "absolute_form needs a scheme");
    debug_assert!(
        uri.authority().is_some(),
        "absolute_form needs an authority"
    );
}

/// Convert the URI to origin-form, if it is not already.
///
/// This form of the URI has no scheme or authority, and contains just
/// the path, usually used in HTTP/1 requests.
fn origin_form(uri: &mut Uri) {
    let path = match uri.path_and_query() {
        Some(path) if path.as_str() != "/" => {
            let mut parts = ::http::uri::Parts::default();
            parts.path_and_query = Some(path.clone());
            Uri::from_parts(parts).expect("path is valid uri")
        }
        _none_or_just_slash => {
            debug_assert!(Uri::default() == "/");
            Uri::default()
        }
    };
    *uri = path
}

/// Returns the port if it is not the default port for the scheme.
fn get_non_default_port(uri: &Uri) -> Option<Port<&str>> {
    match (uri.port().map(|p| p.as_u16()), is_schema_secure(uri)) {
        (Some(443), true) => None,
        (Some(80), false) => None,
        _ => uri.port(),
    }
}

/// Returns true if the URI scheme is presumed secure.
fn is_schema_secure(uri: &Uri) -> bool {
    uri.scheme_str()
        .map(|scheme_str| matches!(scheme_str, "wss" | "https"))
        .unwrap_or_default()
}

/// Set the Host header on the request if it is not already set,
/// using the authority from the URI.
fn set_host_header<B>(request: &mut http::Request<B>) {
    let uri = request.uri().clone();
    request
        .headers_mut()
        .entry(http::header::HOST)
        .or_insert_with(|| {
            let hostname = uri.host().expect("authority implies host");
            if let Some(port) = get_non_default_port(&uri) {
                let s = format!("{}:{}", hostname, port);
                HeaderValue::from_str(&s)
            } else {
                HeaderValue::from_str(hostname)
            }
            .expect("uri host is valid header value")
        });
}

#[cfg(test)]
mod tests {

    #[cfg(feature = "mocks")]
    use crate::Body;

    #[cfg(feature = "mocks")]
    use self::conn::protocol::mock::MockProtocol;
    #[cfg(feature = "mocks")]
    use self::conn::transport::mock::{MockConnectionError, MockTransport};

    use super::*;

    #[test]
    fn test_set_host_header() {
        let mut request = http::Request::new(());
        *request.uri_mut() = "http://example.com".parse().unwrap();
        set_host_header(&mut request);
        assert_eq!(
            request.headers().get(http::header::HOST).unwrap(),
            "example.com"
        );

        let mut request = http::Request::new(());
        *request.uri_mut() = "http://example.com:8080".parse().unwrap();
        set_host_header(&mut request);
        assert_eq!(
            request.headers().get(http::header::HOST).unwrap(),
            "example.com:8080"
        );

        let mut request = http::Request::new(());
        *request.uri_mut() = "https://example.com".parse().unwrap();
        set_host_header(&mut request);
        assert_eq!(
            request.headers().get(http::header::HOST).unwrap(),
            "example.com"
        );

        let mut request = http::Request::new(());
        *request.uri_mut() = "https://example.com:8443".parse().unwrap();
        set_host_header(&mut request);
        assert_eq!(
            request.headers().get(http::header::HOST).unwrap(),
            "example.com:8443"
        );
    }

    #[test]
    fn test_is_schema_secure() {
        let uri = "http://example.com".parse().unwrap();
        assert!(!is_schema_secure(&uri));

        let uri = "https://example.com".parse().unwrap();
        assert!(is_schema_secure(&uri));

        let uri = "ws://example.com".parse().unwrap();
        assert!(!is_schema_secure(&uri));

        let uri = "wss://example.com".parse().unwrap();
        assert!(is_schema_secure(&uri));
    }

    #[test]
    fn test_get_non_default_port() {
        let uri = "http://example.com".parse().unwrap();
        assert_eq!(get_non_default_port(&uri).map(|p| p.as_u16()), None);

        let uri = "http://example.com:8080".parse().unwrap();
        assert_eq!(get_non_default_port(&uri).map(|p| p.as_u16()), Some(8080));

        let uri = "https://example.com".parse().unwrap();
        assert_eq!(get_non_default_port(&uri).map(|p| p.as_u16()), None);

        let uri = "https://example.com:8443".parse().unwrap();
        assert_eq!(get_non_default_port(&uri).map(|p| p.as_u16()), Some(8443));
    }

    #[test]
    fn test_origin_form() {
        let mut uri = "http://example.com".parse().unwrap();
        origin_form(&mut uri);
        assert_eq!(uri, "/");

        let mut uri = "/some/path/here".parse().unwrap();
        origin_form(&mut uri);
        assert_eq!(uri, "/some/path/here");

        let mut uri = "http://example.com:8080/some/path?query#fragment"
            .parse()
            .unwrap();
        origin_form(&mut uri);
        assert_eq!(uri, "/some/path?query");

        let mut uri = "/".parse().unwrap();
        origin_form(&mut uri);
        assert_eq!(uri, "/");
    }

    #[test]
    fn test_absolute_form() {
        let mut uri = "http://example.com".parse().unwrap();
        absolute_form(&mut uri);
        assert_eq!(uri, "http://example.com");

        let mut uri = "http://example.com:8080".parse().unwrap();
        absolute_form(&mut uri);
        assert_eq!(uri, "http://example.com:8080");

        let mut uri = "https://example.com/some/path?query".parse().unwrap();
        absolute_form(&mut uri);
        assert_eq!(uri, "https://example.com/some/path?query");

        let mut uri = "https://example.com:8443".parse().unwrap();
        absolute_form(&mut uri);
        assert_eq!(uri, "https://example.com:8443");

        let mut uri = "http://example.com:443".parse().unwrap();
        absolute_form(&mut uri);
        assert_eq!(uri, "http://example.com:443");

        let mut uri = "https://example.com:80".parse().unwrap();
        absolute_form(&mut uri);
        assert_eq!(uri, "https://example.com:80");
    }

    #[cfg(feature = "mocks")]
    #[tokio::test]
    async fn test_client_mock_transport() {
        let transport = MockTransport::new(false);
        let protocol = MockProtocol;
        let pool = PoolConfig::default();

        let mut client: Client<ClientService<MockTransport, MockProtocol, Body>> =
            Client::new(protocol, transport, pool);

        client
            .request(
                http::Request::builder()
                    .uri("mock://somewhere")
                    .body(crate::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    }

    #[cfg(feature = "mocks")]
    #[tokio::test]
    async fn test_client_mock_connection_error() {
        let transport = MockTransport::connection_error();
        let protocol = MockProtocol;
        let pool = PoolConfig::default();

        let mut client: Client<ClientService<MockTransport, MockProtocol, Body>> =
            Client::new(protocol, transport, pool);

        let result = client
            .request(
                http::Request::builder()
                    .uri("mock://somewhere")
                    .body(crate::Body::empty())
                    .unwrap(),
            )
            .await;

        let err = result.unwrap_err();

        let Error::Connection(err) = err else {
            panic!("unexpected error: {:?}", err);
        };

        let err = err.downcast::<ConnectionError>().unwrap();

        let ConnectionError::Connecting(err) = *err else {
            panic!("unexpected error: {:?}", err);
        };

        err.downcast::<MockConnectionError>().unwrap();
    }
}
