use std::future::poll_fn;
use std::task::Poll;

use http::uri::Port;
use http::uri::Scheme;
use http::HeaderValue;
use http::Uri;
use http::Version;
use tower::util::Oneshot;
use tower::ServiceExt;
use tracing::warn;

use self::future::ResponseFuture;

use super::conn::connection::ConnectionError;
use super::conn::protocol::auto::HttpConnectionBuilder;
use super::conn::protocol::HttpProtocol;
use super::conn::transport::tcp::TcpTransport;
use super::conn::transport::TransportStream;
use super::conn::Connection;
use super::conn::Protocol;
use super::conn::TlsTransport;
use super::conn::Transport;
use super::pool;
use super::pool::Checkout;
use super::pool::Connector;
use super::pool::PoolableConnection;
use super::pool::Pooled;
use super::Error;
use crate::info::HasConnectionInfo;

mod future;

/// A client which provides a simple HTTP `tower::Service`.
///
/// Client Services combine a [transport][Transport] (e.g. TCP) and a [protocol][Protocol] (e.g. HTTP)
/// to provide a `tower::Service` that can be used to make requests to a server. Optionally, a connection
/// pool can be configured so that individual connections can be reused.
///
/// To use a client service, you must first poll the service to readiness with `Service::poll_ready`,
/// and then make the request with `Service::call`. This can be simplified with the `tower::ServiceExt`
/// which provides a `Service::oneshot` method that combines these two steps into a single future.
#[derive(Debug)]
pub struct ClientService<T, P, BOut = crate::Body>
where
    T: Transport,
    P: Protocol<T::IO>,
    P::Connection: PoolableConnection,
{
    pub(super) transport: T,
    pub(super) protocol: P,
    pub(super) pool: Option<pool::Pool<P::Connection>>,
    pub(super) _body: std::marker::PhantomData<fn() -> BOut>,
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

    /// Disable connection pooling for this client.
    pub fn without_pool(self) -> Self {
        Self { pool: None, ..self }
    }
}

impl ClientService<TlsTransport<TcpTransport>, HttpConnectionBuilder, crate::Body> {
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

impl<P, C, T, BOut> ClientService<T, P, BOut>
where
    C: Connection + PoolableConnection,
    P: Protocol<T::IO, Connection = C, Error = ConnectionError> + Clone + Send + Sync + 'static,
    T: Transport + 'static,
    T::IO: Unpin,
    BOut: From<crate::body::Body> + Unpin,
    <<T as Transport>::IO as HasConnectionInfo>::Addr: Send,
    C::ResBody: Into<crate::Body>,
{
    /// Send an http Request, and return a Future of the Response.
    pub fn request(
        &self,
        request: crate::body::Request,
    ) -> Oneshot<Self, http::Request<crate::Body>> {
        self.clone().oneshot(request)
    }
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
        if request.version() == Version::HTTP_2 || request.version() == Version::HTTP_3 {
            warn!(
                "refusing to send {:?} request to HTTP/1.1 connection",
                request.version()
            );
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
        *request.version_mut() = Version::HTTP_2;
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
    use crate::client::conn::protocol::mock::MockProtocol;
    #[cfg(feature = "mocks")]
    use crate::client::conn::transport::mock::{MockConnectionError, MockTransport};

    use crate::client::pool::Config as PoolConfig;

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

        let client: ClientService<MockTransport, MockProtocol, Body> =
            ClientService::new(transport, protocol, pool);

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

        let client: ClientService<MockTransport, MockProtocol, Body> =
            ClientService::new(transport, protocol, pool);

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
