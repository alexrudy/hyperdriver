//! Dynamic support for in-process gRPC services based on tonic.

#![warn(missing_docs)]
#![warn(missing_debug_implementations)]
#![deny(unsafe_code)]

use core::fmt;
use std::borrow::Cow;
use std::io;
use std::sync::Arc;

use crate::bridge::rt::TokioExecutor;
#[cfg(feature = "client")]
use crate::client::conn::TlsTransport;
use crate::client::conn::TransportTlsExt;
#[cfg(feature = "client")]
use crate::client::HttpConnectionBuilder;
use crate::pidfile::PidFile;
use crate::server::AutoBuilder;
#[cfg(feature = "client")]
use crate::stream::client::Stream as ClientStream;

use camino::{Utf8Path, Utf8PathBuf};
use dashmap::mapref::one::{Ref, RefMut};
use dashmap::DashMap;
use hyper::Uri;
use tower::make::Shared;

mod transport;

pub use transport::GrpcScheme;
pub use transport::RegistryTransport;
pub use transport::Scheme;
pub use transport::SvcScheme;
pub use transport::TransportBuilder;

/// Service Registry client which will connect to internal services.

pub type Client<B = crate::body::Body> =
    crate::client::Client<HttpConnectionBuilder, TlsTransport<transport::RegistryTransport>, B>;

/// An error occured while connecting to a service.
#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    /// The service name was not a valid authority (e.g. `svc://foo`)
    #[error("Invalid name: {0}")]
    InvalidName(String),

    /// The service is not available. It may have been previously closed.
    #[error("Service {0} is not available")]
    ServiceClosed(String),

    /// Connection to the service timed out.
    #[error("Connection to {0} timed out")]
    ConnectionTimeout(String, #[source] tokio::time::error::Elapsed),

    /// The service URI is not a valid URI.
    #[error("Invalid URI: {0}")]
    InvalidUri(Uri),

    /// An IO error occured while connecting to the service.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Internal error when something goes wrong during Bind.
///
/// Doesn't require the name, it will be added in context
/// farther up.
#[derive(Debug)]
pub(crate) enum InternalBindError {
    AlreadyBound,

    SocketResetError(Utf8PathBuf, io::Error),

    PidLockError(Utf8PathBuf, io::Error),
}

/// An error occured binding this service to the specified name.
#[derive(Debug)]
pub struct BindError {
    service: String,
    inner: InternalBindError,
}

impl BindError {
    fn new(service: String, inner: InternalBindError) -> Self {
        Self { service, inner }
    }
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            InternalBindError::AlreadyBound => {
                write!(f, "Service {} is already bound", self.service)
            }
            InternalBindError::SocketResetError(path, error) => {
                write!(
                    f,
                    "Service {}: Unable to reset socket at {}: {}",
                    self.service, path, error
                )
            }
            InternalBindError::PidLockError(path, error) => {
                write!(
                    f,
                    "Service {}: Unable to lock PID file at {}: {}",
                    self.service, path, error
                )
            }
        }
    }
}

impl std::error::Error for BindError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.inner {
            InternalBindError::AlreadyBound => None,
            InternalBindError::SocketResetError(_, error) => Some(error),
            InternalBindError::PidLockError(_, error) => Some(error),
        }
    }
}

/// Service discovery mechanism for services registered.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceDiscovery {
    /// Discover services in the same process, using an in-memory store and transport.
    #[default]
    InProcess,

    /// Discover services by looking for a well-known unix socket.
    Unix {
        /// Path to the directory containing the unix sockets.
        path: Utf8PathBuf,
    },
}

/// Configuration for the service registry.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct RegistryConfig {
    /// Service discovery mechanism.
    pub service_discovery: ServiceDiscovery,

    /// Connection timeout when finding a service.
    #[serde(with = "humantime_serde")]
    pub connect_timeout: Option<std::time::Duration>,

    /// Buffer size for in-memory transports.
    pub buffer_size: usize,

    /// Proxy service timeout
    #[serde(with = "humantime_serde")]
    pub proxy_timeout: std::time::Duration,

    /// Proxy concurrency limit
    pub proxy_limit: usize,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            service_discovery: Default::default(),
            connect_timeout: None,
            buffer_size: 1024 * 1024,
            proxy_timeout: std::time::Duration::from_secs(30),
            proxy_limit: 32,
        }
    }
}

/// Maintains the set of available services, and the connection
/// configurations for those services.
#[derive(Clone, Default)]
pub struct ServiceRegistry {
    inner: Arc<InnerRegistry>,
    config: Arc<RegistryConfig>,
}

impl std::fmt::Debug for ServiceRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceRegistry")
            .field("config", &self.config)
            .finish()
    }
}

impl ServiceRegistry {
    /// Create a new registry with default configuration.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(InnerRegistry::default()),
            config: Arc::new(RegistryConfig::default()),
        }
    }

    /// Create a new registry with the specified configuration.
    pub fn new_with_config(config: RegistryConfig) -> Self {
        Self {
            inner: Arc::new(InnerRegistry::default()),
            config: Arc::new(config),
        }
    }

    #[inline]
    fn config_mut(&mut self) -> &mut RegistryConfig {
        Arc::make_mut(&mut self.config)
    }

    /// Set the service discovery mechanism.
    pub fn set_discovery(&mut self, discovery: ServiceDiscovery) {
        self.config_mut().service_discovery = discovery;
    }

    /// Set the connection timeout for finding a service.
    pub fn set_connect_timeout(&mut self, timeout: std::time::Duration) {
        self.config_mut().connect_timeout = Some(timeout);
    }

    /// Set the buffer size for in-memory transports.
    ///
    /// See [`crate::stream::duplex::DuplexClient::connect`] for more information.
    pub fn set_buffer_size(&mut self, size: usize) {
        self.config_mut().buffer_size = size;
    }

    /// Check if a service is available, by name.
    pub fn is_available<S: AsRef<str>>(&self, service: S) -> bool {
        self.inner.is_available(&self.config, service.as_ref())
    }

    /// Get an acceptor which will be bound to a service with this name.
    #[tracing::instrument(skip_all, fields(service=tracing::field::Empty))]
    pub async fn bind<'a, S>(
        &'a self,
        service: S,
    ) -> Result<crate::stream::server::Acceptor, BindError>
    where
        S: Into<Cow<'a, str>>,
    {
        let name = service.into();
        let span = tracing::Span::current();
        span.record("service", name.as_ref());

        self.inner
            .bind(&self.config, &name)
            .map_err(|err| BindError::new(name.into_owned(), err))
    }

    /// Create a server which will bind to a service by name.
    pub async fn server<'a, M, S, B>(
        &'a self,
        make_service: M,
        name: S,
    ) -> Result<
        crate::server::Server<M, AutoBuilder<TokioExecutor>, crate::stream::server::Acceptor, B>,
        BindError,
    >
    where
        S: Into<Cow<'a, str>>,
    {
        let acceptor = self.bind(name.into()).await?;
        Ok(crate::server::Server::new(acceptor, make_service))
    }

    /// Create a server which will use a registry transport to proxy requests to services.
    pub fn router<A, B>(
        &self,
        acceptor: A,
    ) -> crate::server::Server<Shared<Client>, AutoBuilder<TokioExecutor>, A, B> {
        crate::server::Server::new(acceptor, Shared::new(self.client()))
    }

    /// Connect to a service by name.
    ///
    /// Prefer using `client` instead of this method.
    #[tracing::instrument(skip_all, fields(service=tracing::field::Empty))]
    pub async fn connect<'a, S: Into<Cow<'a, str>>>(
        &'a self,
        service: S,
    ) -> Result<ClientStream, ConnectionError> {
        let service = service.into();
        let span = tracing::Span::current();
        span.record("service", service.as_ref());

        self.inner.connect(&self.config, service).await
    }

    /// Create a transport for internal services, with default schemes.
    ///
    /// The default schemes are `grpc` and `svc`. `svc` uses the host to determine the service, and `grpc` uses the
    /// first path component, and is suitable for gRPC services.
    pub fn default_transport(&self) -> transport::RegistryTransport {
        transport::RegistryTransport::with_default_schemes(self.clone())
    }

    /// Create a transport builder for internal services.
    pub fn transport_builder(&self) -> transport::TransportBuilder {
        transport::RegistryTransport::builder(self.clone())
    }

    /// Create a client which will connect to internal services.
    pub fn client<B>(&self) -> Client<B> {
        let transport = self.default_transport().without_tls();
        Client::new(Default::default(), transport, Default::default())
    }
}

/// Maintains the set of available services
#[derive(Debug)]
struct InnerRegistry {
    services: DashMap<String, ServiceHandle>,
}

impl Default for InnerRegistry {
    fn default() -> Self {
        Self {
            services: DashMap::new(),
        }
    }
}

impl InnerRegistry {
    fn get_mut(&self, config: &RegistryConfig, service: &str) -> RefMut<'_, String, ServiceHandle> {
        self.services
            .entry(service.to_owned())
            .or_insert_with(|| match &config.service_discovery {
                ServiceDiscovery::InProcess => ServiceHandle::duplex(service),
                ServiceDiscovery::Unix { path } => ServiceHandle::unix(path, service),
            })
    }

    fn get(&self, config: &RegistryConfig, service: &str) -> Ref<'_, String, ServiceHandle> {
        if let Some(handle) = self.services.get(service) {
            handle
        } else {
            self.get_mut(config, service).downgrade()
        }
    }

    fn is_available(&self, config: &RegistryConfig, service: &str) -> bool {
        let handle = self.get(config, service);
        handle.is_available()
    }

    /// Connect to a service by name.
    #[tracing::instrument(skip(self, config))]
    async fn connect(
        &self,
        config: &RegistryConfig,
        service: Cow<'_, str>,
    ) -> Result<ClientStream, ConnectionError> {
        let handle = self.get(config, service.as_ref());

        connect_to_handle(config, handle.value(), service).await
    }

    /// Bind to a service by name.
    fn bind(
        &self,
        config: &RegistryConfig,
        service: &str,
    ) -> Result<crate::stream::server::Acceptor, InternalBindError> {
        let mut handle = self.get_mut(config, service);

        handle.acceptor()
    }
}

/// Represents a discovered service which uses a PID file to lock binding the service.
#[derive(Debug)]
enum PidLock {
    Path(Utf8PathBuf),

    #[allow(dead_code)]
    Lock(PidFile),
}

impl PidLock {
    fn is_available(&self) -> bool {
        tracing::trace!("Checking PID file {self:?}");
        match self {
            PidLock::Path(path) => PidFile::is_locked(path)
                .map_err(|error| tracing::warn!("Unable to inspect PID file: {error:?}"))
                .unwrap_or(false),
            PidLock::Lock(_) => true,
        }
    }
}

/// Handle to a service for creating new connections
///
/// This is the type held internally by the registry for a service.
#[derive(Debug)]
enum ServiceHandle {
    Duplex {
        acceptor: Option<crate::stream::server::Acceptor>,
        connector: crate::stream::duplex::DuplexClient,
    },
    Unix {
        path: Utf8PathBuf,
        pidfile: PidLock,
    },
}

impl ServiceHandle {
    fn duplex(service: &str) -> Self {
        let (connector, acceptor) = crate::stream::duplex::pair(service.parse().unwrap());
        Self::Duplex {
            acceptor: Some(acceptor.into()),
            connector,
        }
    }

    fn unix(path: &Utf8Path, service: &str) -> Self {
        let svcpath = path.join(format!("{service}.svc"));
        let pidfile = path.join(format!("{service}.pid"));

        Self::Unix {
            path: svcpath,
            pidfile: PidLock::Path(pidfile),
        }
    }

    fn is_available(&self) -> bool {
        match self {
            ServiceHandle::Duplex { acceptor, .. } => acceptor.is_none(),
            ServiceHandle::Unix { pidfile, .. } => pidfile.is_available(),
        }
    }

    async fn connect(
        &self,
        config: &RegistryConfig,
    ) -> Result<crate::stream::client::Stream, io::Error> {
        match self {
            ServiceHandle::Duplex { connector, .. } => {
                Ok(connector.connect(config.buffer_size, None).await?.into())
            }
            ServiceHandle::Unix { path, .. } => tokio::net::UnixStream::connect(path)
                .await
                .map(|stream| stream.into()),
        }
    }

    /// Create an acceptor for this service.
    fn acceptor(&mut self) -> Result<crate::stream::server::Acceptor, InternalBindError> {
        match self {
            ServiceHandle::Duplex { acceptor, .. } => {
                tracing::trace!("Preparing in-process acceptor");
                acceptor.take().ok_or(InternalBindError::AlreadyBound)
            }
            ServiceHandle::Unix { ref path, pidfile } => {
                tracing::trace!("Locking PID file");
                let file = match pidfile {
                    PidLock::Path(ref path) => PidFile::new(path.clone()).map_err(|err| {
                        tracing::warn!(
                            "Encountered an error resetting the Pid file {path}: {}",
                            err
                        );
                        InternalBindError::PidLockError(path.clone(), err)
                    })?,
                    PidLock::Lock(_) => {
                        tracing::warn!("Service is already bound in this process");
                        return Err(InternalBindError::AlreadyBound);
                    }
                };
                *pidfile = PidLock::Lock(file);

                tracing::trace!("Binding to socket at {path}");
                if let Err(error) = std::fs::remove_file(path) {
                    match error.kind() {
                        io::ErrorKind::NotFound => {}
                        _ => {
                            tracing::error!("Unable to remove socket: {}", error);
                            return Err(InternalBindError::SocketResetError(path.clone(), error));
                        }
                    }
                }

                tokio::net::UnixListener::bind(path)
                    .map(|listener| listener.into())
                    .map_err(|error| match error.kind() {
                        io::ErrorKind::AddrInUse => {
                            tracing::warn!("Service is already bound");
                            InternalBindError::AlreadyBound
                        }
                        _ => {
                            tracing::error!("Unable to bind socket: {}", error);
                            InternalBindError::SocketResetError(path.clone(), error)
                        }
                    })
            }
        }
    }
}

async fn connect_to_handle(
    config: &RegistryConfig,
    handle: &ServiceHandle,
    name: Cow<'_, str>,
) -> Result<ClientStream, ConnectionError> {
    let request = handle.connect(config);

    let stream = if let Some(timeout) = &config.connect_timeout {
        tracing::trace!("Waiting for connection to {name} with timeout");
        match tokio::time::timeout(*timeout, request).await {
            Ok(outcome) => outcome,
            Err(elapsed) => {
                tracing::warn!(
                    "Connection to {name} timed out after {timeout:?}",
                    name = name,
                    timeout = elapsed
                );
                return Err(ConnectionError::ConnectionTimeout(
                    name.into_owned(),
                    elapsed,
                ));
            }
        }
    } else {
        tracing::trace!("Waiting for connection to {name} without timeout");

        // Pin the request future so it can be polled in two places: once during
        // the timeout, and once after the timeout.
        tokio::pin!(request);

        // Apply a default timeout so we can warn when a service is taking a long time
        let default_timeout = std::time::Duration::from_secs(30);
        match tokio::time::timeout(default_timeout, &mut request).await {
            Ok(Ok(stream)) => Ok(stream),
            Err(_) => {
                tracing::warn!(
                    "Waited {}s without a timeout for connection to {name}... continuing",
                    default_timeout.as_secs()
                );
                request.await
            }
            Ok(Err(error)) => Err(error),
        }
    }
    .map_err(|err| {
        tracing::warn!("failed to complete connection: {}", err);
        ConnectionError::ServiceClosed(name.into_owned())
    })?;

    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_handle() {
        let tmp = tempfile::tempdir().unwrap();
        let name = "service.with.dots";
        let handle = ServiceHandle::unix(tmp.path().try_into().unwrap(), name);

        assert!(!handle.is_available());

        let ServiceHandle::Unix { path, pidfile } = handle else {
            panic!("expected unix handle")
        };

        let expected =
            Utf8PathBuf::from_path_buf(tmp.path().join(format!("{}.svc", name))).unwrap();

        assert_eq!(path, expected);
        assert!(matches!(pidfile, PidLock::Path(_)));
    }
}
