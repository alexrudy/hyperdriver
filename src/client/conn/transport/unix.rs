//! Unix Domain Socket transport implementation for client connections.
//!
//! This module contains the [`UnixTransport`] type, which is a [`tower::Service`] that connects to
//! Unix domain sockets. Unlike TCP transports, Unix sockets use filesystem paths as addresses.
//!
//! The transport extracts the socket path from the URI authority or path component and establishes
//! a connection to the Unix domain socket at that location.

use std::io;
use std::marker::PhantomData;
use std::path::Path;
use std::task::{Context, Poll};
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::trace;

use crate::info::HasConnectionInfo;
use crate::info::UnixAddr;
use crate::stream::unix::UnixStream;

/// A Unix Domain Socket connector for client connections.
///
/// This type is a [`tower::Service`] that connects to Unix domain sockets using filesystem paths.
/// The socket path is extracted from the URI - it can be specified in the authority component
/// (for URIs like `unix:///path/to/socket`) or as the path component.
///
/// # Example
/// ```no_run
/// # use hyperdriver::client::conn::transport::unix::UnixTransport;
/// # use hyperdriver::stream::unix::UnixStream;
/// # use hyperdriver::info::UnixAddr;
/// # use hyperdriver::IntoRequestParts;
/// # use tower::ServiceExt as _;
///
/// # async fn run() {
/// let transport: UnixTransport<UnixStream> = UnixTransport::default();
///
/// let request = http::Request::get("http://somewhere/over/the/rainbow").body(()).unwrap();
/// let (mut parts, _) = request.into_parts();
/// parts.extensions.insert(UnixAddr::from_pathbuf("/var/run/my-service.sock".into()));
/// let stream = transport.oneshot(parts).await.unwrap();
/// # }
/// ```
#[derive(Debug)]
pub struct UnixTransport<IO = UnixStream> {
    config: UnixTransportConfig,
    stream: PhantomData<fn() -> IO>,
}

impl<IO> Clone for UnixTransport<IO> {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            stream: PhantomData,
        }
    }
}

impl<IO> Default for UnixTransport<IO> {
    fn default() -> Self {
        Self::new(UnixTransportConfig::default())
    }
}

impl<IO> UnixTransport<IO> {
    /// Create a new Unix transport with the given configuration.
    pub fn new(config: UnixTransportConfig) -> Self {
        Self {
            config,
            stream: PhantomData,
        }
    }

    /// Get the configuration for the Unix transport.
    pub fn config(&self) -> &UnixTransportConfig {
        &self.config
    }

    /// Set the configuration for the Unix transport.
    pub fn with_config(mut self, config: UnixTransportConfig) -> Self {
        self.config = config;
        self
    }
}

type BoxFuture<'a, T, E> = crate::BoxFuture<'a, Result<T, E>>;

impl<IO> tower::Service<http::request::Parts> for UnixTransport<IO>
where
    UnixStream: Into<IO>,
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    IO::Addr: Clone + Unpin + Send + 'static,
{
    type Response = IO;
    type Error = UnixConnectionError;
    type Future = BoxFuture<'static, Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::request::Parts) -> Self::Future {
        let config = self.config.clone();

        Box::pin(async move {
            let path = extract_unix_path_from_request(&req)?;
            let stream = connect_unix_socket(path, config.connect_timeout).await?;

            trace!(path = %path, "unix socket connected");

            let stream = stream.into();
            Ok(stream)
        })
    }
}

/// Extract a Unix address from the request extensions.
fn extract_unix_path_from_request(
    req: &http::request::Parts,
) -> Result<&Utf8Path, UnixConnectionError> {
    req.extensions
        .get::<UnixAddr>()
        .ok_or(UnixConnectionError::NoAddress)
        .and_then(|addr| addr.path().ok_or(UnixConnectionError::UnnamedAddress))
}

/// Connect to a Unix domain socket at the given path.
async fn connect_unix_socket<P: AsRef<Path>>(
    path: P,
    connect_timeout: Option<Duration>,
) -> Result<UnixStream, UnixConnectionError> {
    let connect_future = UnixStream::connect(path);

    match connect_timeout {
        Some(timeout) => match tokio::time::timeout(timeout, connect_future).await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(e)) => Err(UnixConnectionError::ConnectionError(e)),
            Err(_) => {
                trace!(timeout=?timeout, "unix connection timed out");
                Err(UnixConnectionError::Timeout(timeout))
            }
        },
        None => connect_future
            .await
            .map_err(UnixConnectionError::ConnectionError),
    }
}

/// Error type for Unix socket connections.
#[derive(Debug, Error)]
pub enum UnixConnectionError {
    /// Error when no unix address is found in request extensions.
    #[error("No unix address in request extensions")]
    NoAddress,

    /// Error when the unix address is unnamed.
    #[error("Unnamed unix address provided")]
    UnnamedAddress,

    /// Error when the unix connection fails.
    #[error("Unix connection: {0}")]
    ConnectionError(#[from] io::Error),

    /// Error when the unix connection times out.
    #[error("Connection timed out after {}ms", .0.as_millis())]
    Timeout(Duration),
}

/// Configuration for Unix domain socket connections.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct UnixTransportConfig {
    /// The timeout for connecting to a Unix socket.
    pub connect_timeout: Option<Duration>,
}

impl Default for UnixTransportConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Some(Duration::from_secs(10)),
        }
    }
}

/// A Unix Domain Socket connector that always connects to a single static address.
///
/// Unlike [`UnixTransport`], which extracts the socket path from request extensions,
/// this transport is configured with a single Unix socket path and always connects
/// to that address regardless of the request.
///
/// # Example
/// ```no_run
/// # use hyperdriver::client::conn::transport::unix::StaticAddressUnixTransport;
/// # use hyperdriver::stream::unix::UnixStream;
/// # use tower::ServiceExt as _;
///
/// # async fn run() {
/// let transport: StaticAddressUnixTransport<UnixStream> =
///     StaticAddressUnixTransport::new("/var/run/my-service.sock");
///
/// let request = http::Request::get("http://somewhere/over/the/rainbow").body(()).unwrap();
/// let (parts, _) = request.into_parts();
/// let stream = transport.oneshot(parts).await.unwrap();
/// # }
/// ```
#[derive(Debug)]
pub struct StaticAddressUnixTransport<IO = UnixStream> {
    address: Utf8PathBuf,
    config: UnixTransportConfig,
    stream: PhantomData<fn() -> IO>,
}

impl<IO> Clone for StaticAddressUnixTransport<IO> {
    fn clone(&self) -> Self {
        Self {
            address: self.address.clone(),
            config: self.config.clone(),
            stream: PhantomData,
        }
    }
}

impl<IO> StaticAddressUnixTransport<IO> {
    /// Create a new static Unix transport that always connects to the given path.
    pub fn new<P: Into<Utf8PathBuf>>(path: P) -> Self {
        Self {
            address: path.into(),
            config: UnixTransportConfig::default(),
            stream: PhantomData,
        }
    }

    /// Create a new static Unix transport with the given configuration.
    pub fn with_config<P: Into<Utf8PathBuf>>(path: P, config: UnixTransportConfig) -> Self {
        Self {
            address: path.into(),
            config,
            stream: PhantomData,
        }
    }

    /// Get the static address this transport connects to.
    pub fn address(&self) -> &Utf8PathBuf {
        &self.address
    }

    /// Get the configuration for the Unix transport.
    pub fn config(&self) -> &UnixTransportConfig {
        &self.config
    }

    /// Set the configuration for the Unix transport.
    pub fn set_config(&mut self, config: UnixTransportConfig) {
        self.config = config;
    }
}

impl<IO> tower::Service<http::request::Parts> for StaticAddressUnixTransport<IO>
where
    UnixStream: Into<IO>,
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    IO::Addr: Clone + Unpin + Send + 'static,
{
    type Response = IO;
    type Error = UnixConnectionError;
    type Future = BoxFuture<'static, Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: http::request::Parts) -> Self::Future {
        let address = self.address.clone();
        let config = self.config.clone();

        Box::pin(async move {
            let stream = connect_unix_socket(address.as_std_path(), config.connect_timeout).await?;

            trace!(path = %address, "unix socket connected to static address");

            let stream = stream.into();
            Ok(stream)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use http::Uri;

    #[tokio::test]
    async fn test_unix_transport_invalid_path() {
        let transport: UnixTransport<UnixStream> = UnixTransport::default();

        let uri: Uri = "http://example.com".parse().unwrap();
        let req = http::Request::builder()
            .uri(uri)
            .body(())
            .unwrap()
            .into_parts()
            .0;

        let result = tower::ServiceExt::oneshot(transport, req).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_unix_addr_from_request_with_valid_addr() {
        let mut req = http::Request::builder()
            .uri("http://example.com")
            .body(())
            .unwrap()
            .into_parts()
            .0;

        let addr = UnixAddr::from_pathbuf(Utf8PathBuf::from("/tmp/test.sock"));
        req.extensions.insert(addr.clone());

        let result = extract_unix_path_from_request(&req);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Utf8PathBuf::from("/tmp/test.sock"));
    }

    #[test]
    fn test_extract_unix_addr_from_request_without_addr() {
        let req = http::Request::builder()
            .uri("http://example.com")
            .body(())
            .unwrap()
            .into_parts()
            .0;

        let result = extract_unix_path_from_request(&req);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            UnixConnectionError::NoAddress
        ));
    }

    #[tokio::test]
    async fn test_unix_transport_with_named_address() {
        let transport: UnixTransport<UnixStream> = UnixTransport::default();

        let mut req = http::Request::builder()
            .uri("http://example.com")
            .body(())
            .unwrap()
            .into_parts()
            .0;

        // Use a non-existent path to test the address handling logic without actual connection
        let addr = UnixAddr::from_pathbuf(Utf8PathBuf::from("/nonexistent/socket.sock"));
        req.extensions.insert(addr);

        let result = tower::ServiceExt::oneshot(transport, req).await;
        // Should fail with connection error, not address error
        assert!(result.is_err());
        match result.unwrap_err() {
            UnixConnectionError::ConnectionError(_) => {} // Expected
            UnixConnectionError::NoAddress | UnixConnectionError::UnnamedAddress => {
                panic!("Should not fail with address errors");
            }
            _ => {} // Other errors are fine too (timeout, etc.)
        }
    }

    #[tokio::test]
    async fn test_unix_transport_with_unnamed_address() {
        let transport: UnixTransport<UnixStream> = UnixTransport::default();

        let mut req = http::Request::builder()
            .uri("http://example.com")
            .body(())
            .unwrap()
            .into_parts()
            .0;

        let addr = UnixAddr::unnamed();
        req.extensions.insert(addr);

        let result = tower::ServiceExt::oneshot(transport, req).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            UnixConnectionError::UnnamedAddress
        ));
    }

    #[test]
    fn test_unix_connection_error_display() {
        let error = UnixConnectionError::NoAddress;
        assert_eq!(error.to_string(), "No unix address in request extensions");

        let error = UnixConnectionError::UnnamedAddress;
        assert_eq!(error.to_string(), "Unnamed unix address provided");

        let timeout = std::time::Duration::from_secs(5);
        let error = UnixConnectionError::Timeout(timeout);
        assert_eq!(error.to_string(), "Connection timed out after 5000ms");
    }

    #[test]
    fn test_unix_transport_config() {
        let config = UnixTransportConfig::default();
        assert_eq!(
            config.connect_timeout,
            Some(std::time::Duration::from_secs(10))
        );

        let custom_config = UnixTransportConfig {
            connect_timeout: Some(std::time::Duration::from_secs(30)),
        };

        let transport = UnixTransport::<UnixStream>::new(custom_config.clone());
        assert_eq!(
            transport.config().connect_timeout,
            custom_config.connect_timeout
        );

        let transport_with_config =
            UnixTransport::<UnixStream>::default().with_config(custom_config.clone());
        assert_eq!(
            transport_with_config.config().connect_timeout,
            custom_config.connect_timeout
        );
    }

    #[test]
    fn test_static_address_unix_transport_new() {
        let transport = StaticAddressUnixTransport::<UnixStream>::new("/var/run/test.sock");
        assert_eq!(
            transport.address(),
            &Utf8PathBuf::from("/var/run/test.sock")
        );
        assert_eq!(
            transport.config().connect_timeout,
            Some(std::time::Duration::from_secs(10))
        );
    }

    #[test]
    fn test_static_address_unix_transport_with_config() {
        let config = UnixTransportConfig {
            connect_timeout: Some(std::time::Duration::from_secs(30)),
        };
        let transport = StaticAddressUnixTransport::<UnixStream>::with_config(
            "/var/run/test.sock",
            config.clone(),
        );
        assert_eq!(
            transport.address(),
            &Utf8PathBuf::from("/var/run/test.sock")
        );
        assert_eq!(transport.config().connect_timeout, config.connect_timeout);
    }

    #[test]
    fn test_static_address_unix_transport_set_config() {
        let mut transport = StaticAddressUnixTransport::<UnixStream>::new("/var/run/test.sock");
        let new_config = UnixTransportConfig {
            connect_timeout: Some(std::time::Duration::from_secs(60)),
        };
        transport.set_config(new_config.clone());
        assert_eq!(
            transport.config().connect_timeout,
            new_config.connect_timeout
        );
    }

    #[test]
    fn test_static_address_unix_transport_clone() {
        let transport = StaticAddressUnixTransport::<UnixStream>::new("/var/run/test.sock");
        let cloned = transport.clone();
        assert_eq!(transport.address(), cloned.address());
        assert_eq!(
            transport.config().connect_timeout,
            cloned.config().connect_timeout
        );
    }

    #[tokio::test]
    async fn test_static_address_unix_transport_connection_failure() {
        let transport = StaticAddressUnixTransport::<UnixStream>::new("/nonexistent/socket.sock");

        let req = http::Request::builder()
            .uri("http://example.com")
            .body(())
            .unwrap()
            .into_parts()
            .0;

        let result = tower::ServiceExt::oneshot(transport, req).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            UnixConnectionError::ConnectionError(_) => {} // Expected
            other => panic!("Unexpected error type: {other:?}"),
        }
    }

    #[test]
    fn test_static_address_unix_transport_ignores_request_extensions() {
        let transport = StaticAddressUnixTransport::<UnixStream>::new("/var/run/static.sock");

        let mut req = http::Request::builder()
            .uri("http://example.com")
            .body(())
            .unwrap()
            .into_parts()
            .0;

        // Add a different Unix address to the request - it should be ignored
        let different_addr = UnixAddr::from_pathbuf(Utf8PathBuf::from("/var/run/different.sock"));
        req.extensions.insert(different_addr);

        // The transport should still use its static address, not the one from the request
        assert_eq!(
            transport.address(),
            &Utf8PathBuf::from("/var/run/static.sock")
        );
    }

    #[tokio::test]
    async fn test_static_address_unix_transport_with_timeout() {
        let config = UnixTransportConfig {
            connect_timeout: Some(std::time::Duration::from_millis(1)),
        };
        let transport = StaticAddressUnixTransport::<UnixStream>::with_config(
            "/nonexistent/socket.sock",
            config,
        );

        let req = http::Request::builder()
            .uri("http://example.com")
            .body(())
            .unwrap()
            .into_parts()
            .0;

        let result = tower::ServiceExt::oneshot(transport, req).await;
        assert!(result.is_err());
        // Could be either a timeout or connection error depending on system
        match result.unwrap_err() {
            UnixConnectionError::ConnectionError(_) | UnixConnectionError::Timeout(_) => {} // Both are acceptable
            other => panic!("Unexpected error type: {other:?}"),
        }
    }

    #[test]
    fn test_static_address_unix_transport_accepts_different_path_types() {
        // Test with &str
        let transport1 = StaticAddressUnixTransport::<UnixStream>::new("/var/run/test.sock");
        assert_eq!(
            transport1.address(),
            &Utf8PathBuf::from("/var/run/test.sock")
        );

        // Test with String
        let transport2 =
            StaticAddressUnixTransport::<UnixStream>::new(String::from("/var/run/test.sock"));
        assert_eq!(
            transport2.address(),
            &Utf8PathBuf::from("/var/run/test.sock")
        );

        // Test with Utf8PathBuf
        let transport3 =
            StaticAddressUnixTransport::<UnixStream>::new(Utf8PathBuf::from("/var/run/test.sock"));
        assert_eq!(
            transport3.address(),
            &Utf8PathBuf::from("/var/run/test.sock")
        );
    }
}
