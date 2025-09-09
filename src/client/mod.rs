//! HTTP client library for Rust, built on top of [hyper].
//!
//! There are three levels of available APIs in this library:
//!
//! 1. The high-level [`Client`] API, which is the most user-friendly and abstracts away most of the details.
//!    It is "batteries-included", and supports features like redirects, retries and timeouts.
//! 2. The [`Service`][ConnectionPoolService] API, which is a lower-level API that allows for more control over the request and response.
//!    It presents a `tower::Service` that can be used to send requests and receive responses, and can be wrapped
//!    by middleware compatible with the tower ecosystem.
//! 3. The [connection][self::conn] API, which is the lowest-level API that allows for direct control over the
//!    transport and protocol layers. This API is useful for implementing custom transports or protocols, but
//!    might be difficult to directly use as a client.
//!

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};

use chateau::client::pool::{Key, KeyError};
use tower::util::Oneshot;
use tower::ServiceExt;

use self::conn::protocol::auto;
use crate::BoxError;
use chateau::client::conn::transport::tcp::TcpTransport;
pub use chateau::client::ConnectionPoolLayer;
pub use chateau::client::ConnectionPoolService;
use chateau::services::SharedService;

mod builder;
pub mod conn;
mod error;

pub use self::error::Error;
pub use builder::Builder;
pub use chateau::client::pool::Config as PoolConfig;

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

/// A boxed service with http::Request and http::Response and symmetric body types
pub type SharedClientService<BIn, BOut> =
    SharedService<http::Request<BIn>, http::Response<BOut>, Error>;

/// Inner type for managing the client service.
#[derive(Clone)]
struct ClientRef {
    service: SharedClientService<crate::Body, crate::Body>,
}

impl ClientRef {
    fn new(service: impl Into<SharedClientService<crate::Body, crate::Body>>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn request(
        &mut self,
        request: http::Request<crate::Body>,
    ) -> Oneshot<SharedClientService<crate::Body, crate::Body>, http::Request<crate::Body>> {
        let service = self.service.clone();
        std::mem::replace(&mut self.service, service).oneshot(request)
    }
}

/// A high-level async HTTP client.
///
/// This client is built on top of the [`Service`][ConnectionPoolService] API and provides a more user-friendly interface,
/// including support for retries, redirects and timeouts.
///
/// # Example
/// ```no_run
/// # use hyperdriver::client::Client;
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let mut client = Client::build_tcp_http().build();
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
        S: Into<SharedClientService<crate::Body, crate::Body>>,
    {
        Client {
            inner: Arc::new(ClientRef::new(service)),
        }
    }

    /// Create a new, empty builder for clients.
    pub fn builder() -> self::builder::Builder<(), (), ()> {
        Builder::new()
    }

    /// Create a new client builder with default settings applied.
    pub fn build_tcp_http() -> self::builder::Builder<
        conn::dns::GaiResolver,
        TcpTransport,
        auto::HttpConnectionBuilder<crate::Body>,
    > {
        Builder::default()
    }

    /// Create a new client with default settings applied for TCP connections
    /// (with TLS support) and HTTP/1.1 or HTTP/2.
    pub fn new_tcp_http() -> Self {
        Builder::default().build()
    }

    /// Unwrap the inner service from the client.
    pub fn into_inner(self) -> SharedClientService<crate::Body, crate::Body> {
        match Arc::try_unwrap(self.inner) {
            Ok(client) => client.service,
            Err(client) => client.service.clone(),
        }
    }
}

impl Client {
    /// Send an http Request, and return a Future of the Response.
    pub fn request(
        &mut self,
        request: http::Request<crate::Body>,
    ) -> Oneshot<SharedClientService<crate::Body, crate::Body>, http::Request<crate::Body>> {
        Arc::make_mut(&mut self.inner).request(request)
    }

    /// Make a GET request to the given URI.
    pub async fn get(&mut self, uri: http::Uri) -> Result<http::Response<crate::Body>, BoxError> {
        let request = http::Request::get(uri.clone())
            .body(crate::body::Body::empty())
            .unwrap();

        let response = self.request(request).await?;
        Ok(response)
    }
}

impl tower::Service<http::Request<crate::Body>> for Client {
    type Response = http::Response<crate::Body>;
    type Error = Error;
    type Future =
        Oneshot<SharedClientService<crate::Body, crate::Body>, http::Request<crate::Body>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        //TODO: What happens if we poll_ready, then clone, then call? The wrong (not ready) service will be used.
        Arc::make_mut(&mut self.inner).service.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<crate::Body>) -> Self::Future {
        Arc::make_mut(&mut self.inner).request(request)
    }
}

/// Pool key which is used to identify a connection - using scheme
/// and authority.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct UriKey(http::uri::Scheme, Option<http::uri::Authority>);

impl fmt::Display for UriKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}://{}",
            self.0,
            self.1.as_ref().map_or("", |a| a.as_str())
        )
    }
}

impl From<(http::uri::Scheme, http::uri::Authority)> for UriKey {
    fn from(value: (http::uri::Scheme, http::uri::Authority)) -> Self {
        Self(value.0, Some(value.1))
    }
}

impl TryFrom<http::Uri> for UriKey {
    type Error = KeyError;

    fn try_from(uri: http::Uri) -> Result<Self, Self::Error> {
        let parts = uri.into_parts();
        let authority = parts.authority.clone();
        let scheme = parts.scheme.clone().ok_or_else(|| {
            KeyError::new(format!(
                "Missing scheme in URI: {}",
                http::Uri::from_parts(parts).unwrap()
            ))
        })?;
        Ok::<_, KeyError>(Self(scheme, authority))
    }
}

impl FromStr for UriKey {
    type Err = KeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let uri = http::Uri::from_str(s).map_err(KeyError::new)?;
        uri.try_into()
    }
}

impl<B> Key<http::Request<B>> for UriKey {
    fn build_key(request: &http::Request<B>) -> Result<Self, KeyError>
    where
        Self: Sized,
    {
        let uri = request.uri().clone();
        let parts = uri.into_parts();
        let authority = parts.authority;
        let scheme = parts
            .scheme
            .ok_or_else(|| KeyError::new(format!("Missing scheme in URI: {}", request.uri())))?;
        Ok::<_, KeyError>(Self(scheme, authority))
    }
}

#[cfg(test)]
pub(crate) mod test_key {

    use super::*;

    #[test]
    fn key_from_uri() {
        let uri = http::Uri::from_static("http://localhost:8080");
        let key: UriKey = uri.try_into().unwrap();
        assert_eq!(key.0, http::uri::Scheme::HTTP);
        assert_eq!(
            key.1,
            Some(http::uri::Authority::from_static("localhost:8080"))
        );
    }

    #[test]
    fn key_display() {
        let key = UriKey(
            http::uri::Scheme::HTTP,
            Some(http::uri::Authority::from_static("localhost:8080")),
        );
        assert_eq!(key.to_string(), "http://localhost:8080");
    }

    #[test]
    fn key_from_tuple() {
        let key: UriKey = (
            http::uri::Scheme::HTTP,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();
        assert_eq!(key.0, http::uri::Scheme::HTTP);
        assert_eq!(
            key.1,
            Some(http::uri::Authority::from_static("localhost:8080"))
        );
    }

    #[test]
    fn key_debug() {
        let key = UriKey(
            http::uri::Scheme::HTTP,
            Some(http::uri::Authority::from_static("localhost:8080")),
        );
        assert_eq!(format!("{key:?}"), "UriKey(\"http\", Some(localhost:8080))");
    }
}

#[cfg(test)]
mod tests {

    use static_assertions::assert_impl_all;

    use crate::Client;

    assert_impl_all!(Client: Send, Sync);
}
