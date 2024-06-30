//! HTTP client library for Rust, built on top of [hyper].

use std::fmt;
use std::sync::Arc;

use thiserror::Error;
use tower::util::Oneshot;
use tower::ServiceExt;

use self::conn::protocol::auto;
use self::conn::transport::tcp::TcpTransportConfig;
pub use self::service::ClientService;
use crate::client::conn::connection::ConnectionError;
use crate::client::conn::protocol::auto::HttpConnectionBuilder;
use crate::client::conn::transport::tcp::TcpTransport;
use crate::client::conn::TlsTransport;
use crate::service::SharedService;

mod builder;
pub mod conn;
pub mod pool;
mod service;

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

/// Client service for TCP connections with TLS and HTTP.
pub type ClientTlsTcpService =
    ClientService<TlsTransport<TcpTransport>, HttpConnectionBuilder, crate::Body>;

/// A boxed service with http::Request and http::Response and symmetric body types
pub type BoxedClientService<B> = SharedService<http::Request<B>, http::Response<B>, Error>;

struct ClientRef {
    service: BoxedClientService<crate::Body>,
}

impl ClientRef {
    fn new(service: impl Into<BoxedClientService<crate::Body>>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn request(
        &self,
        request: crate::body::Request,
    ) -> Oneshot<BoxedClientService<crate::Body>, http::Request<crate::Body>> {
        self.service.clone().oneshot(request)
    }
}

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
/// let client = Client::build_tcp_http().build();
/// let response = client.get("http://example.com".parse().unwrap()).await.unwrap();
/// println!("Response: {:?}", response);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientRef>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").finish()
    }
}

impl Default for Client {
    fn default() -> Self {
        Builder::default().build()
    }
}

impl Client {
    /// Create a new client from a raw service.
    ///
    /// It is much easier to use the builder interface to create a client.
    pub fn new_from_service<S>(service: S) -> Self
    where
        S: Into<BoxedClientService<crate::Body>>,
    {
        Client {
            inner: Arc::new(ClientRef::new(service)),
        }
    }

    /// Create a new, empty builder for clients.
    pub fn builder() -> self::builder::Builder<(), ()> {
        Builder::new()
    }

    /// Create a new client builder with default settings applied.
    pub fn build_tcp_http(
    ) -> self::builder::Builder<TcpTransportConfig, auto::HttpConnectionBuilder> {
        Builder::default()
    }
}

impl Client {
    /// Send an http Request, and return a Future of the Response.
    pub fn request(
        &self,
        request: crate::body::Request,
    ) -> Oneshot<BoxedClientService<crate::Body>, http::Request<crate::Body>> {
        self.inner.request(request)
    }

    /// Make a GET request to the given URI.
    pub async fn get(&self, uri: http::Uri) -> Result<http::Response<crate::Body>, Error> {
        let request = http::Request::get(uri.clone())
            .body(crate::body::Body::empty())
            .unwrap();

        let response = self.request(request).await?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {

    use static_assertions::assert_impl_all;

    use crate::Client;

    assert_impl_all!(Client: Send, Sync);
}
