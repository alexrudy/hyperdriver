//! Dynamic support for in-process gRPC services based on tonic.

#![warn(missing_docs)]
#![warn(missing_debug_implementations)]
#![deny(unsafe_code)]

use core::fmt;
use std::borrow::Cow;
use std::io;
use std::sync::Arc;

use braid::client::Stream as ClientStream;
use camino::{Utf8Path, Utf8PathBuf};
use dashmap::mapref::one::{Ref, RefMut};
use dashmap::DashMap;
use futures_util::{future::BoxFuture, FutureExt};
use hyper::Uri;
use patron::{HttpConnector, TransportStream};
use pidfile::PidFile;

mod server;
pub use server::GrpcRouter;

/// Service Registry client which will connect to internal services.
pub type Client = patron::Client<HttpConnector, RegistryTransport>;

const GRPC_PROXY_NAME: &str = "grpc-proxy";

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
    /// See [`braid::duplex::DuplexClient::connect`] for more information.
    pub fn set_buffer_size(&mut self, size: usize) {
        self.config_mut().buffer_size = size;
    }

    /// Check if a service is available, by name.
    pub fn is_available<'r, S: Into<Cow<'r, str>>>(&'r self, service: S) -> bool {
        self.inner
            .is_available(&self.config, service.into().as_ref())
    }

    /// Get an acceptor which will be bound to a service with this name.
    ///
    /// Prefer using `server` instead of this method.
    #[tracing::instrument(skip_all, fields(service=%service))]
    pub async fn bind(&self, service: Cow<'_, str>) -> Result<braid::server::Acceptor, BindError> {
        self.inner
            .bind(&self.config, service.clone())
            .map_err(|err| BindError::new(service.into_owned(), err))
    }

    /// Connect to a service by name.
    ///
    /// Prefer using `client` instead of this method.
    #[tracing::instrument(skip_all, fields(service=%service))]
    pub async fn connect(&self, service: Cow<'_, str>) -> Result<ClientStream, ConnectionError> {
        self.inner.connect(&self.config, service).await
    }

    /// Provide a transport for internal services.
    pub fn transport(&self) -> RegistryTransport {
        RegistryTransport::new(self.clone())
    }

    /// Create a client which will connect to internal services.
    pub fn client(&self) -> Client {
        Client::new(
            HttpConnector::new(Default::default()),
            self.transport(),
            Default::default(),
        )
    }

    /// Create a router which will multiplex GRPC services.
    pub fn router(&self) -> GrpcRouter {
        GrpcRouter::new(self.clone())
    }
}

/// A connection transport which uses roomservice's internal service registry.
#[derive(Debug, Clone)]
pub struct RegistryTransport {
    registry: ServiceRegistry,
}

impl RegistryTransport {
    fn new(registry: ServiceRegistry) -> Self {
        Self { registry }
    }

    /// The inner service registry
    pub fn registry(&self) -> &ServiceRegistry {
        &self.registry
    }
}

impl Default for RegistryTransport {
    fn default() -> Self {
        Self::new(ServiceRegistry::new())
    }
}

impl tower::Service<Uri> for RegistryTransport {
    type Response = TransportStream;

    type Error = ConnectionError;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Uri) -> Self::Future {
        match req.scheme_str() {
            // SVC proxy support, where the service name is in the host.
            // Exposing this to the internet could expose the list of hosts.
            Some("svc") => {
                let inner = self.registry.inner.clone();
                let config = self.registry.config.clone();
                (async move {
                    let service = req.host().unwrap_or_default();
                    let stream = inner.connect(&config, service.into()).await?;
                    Ok(TransportStream::new(stream)
                        .await
                        .expect("transport failed to handshake"))
                })
                .boxed()
            }

            // GRPC proxy support, where we ignore the host and use the GRPC path to multiplex to the
            // correct upstream service.
            Some("grpc") => {
                let inner = self.registry.inner.clone();
                let config = self.registry.config.clone();
                (async move {
                    let stream =
                        connect_to_handle(&config, &inner.proxy, GRPC_PROXY_NAME.into()).await?;
                    Ok(TransportStream::new(stream)
                        .await
                        .expect("transport failed to handshake"))
                })
                .boxed()
            }
            _ => futures_util::future::ready(Err(ConnectionError::InvalidUri(req))).boxed(),
        }
    }
}

/// Maintains the set of available services
#[derive(Debug)]
struct InnerRegistry {
    services: DashMap<String, ServiceHandle>,
    proxy: ServiceHandle,
}

impl Default for InnerRegistry {
    fn default() -> Self {
        Self {
            services: DashMap::new(),
            proxy: ServiceHandle::duplex(GRPC_PROXY_NAME),
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

    #[tracing::instrument(skip(self, config))]
    async fn connect(
        &self,
        config: &RegistryConfig,
        service: Cow<'_, str>,
    ) -> Result<ClientStream, ConnectionError> {
        let handle = self.get(config, service.as_ref());

        connect_to_handle(config, &handle, service).await
    }

    fn bind(
        &self,
        config: &RegistryConfig,
        service: Cow<'_, str>,
    ) -> Result<braid::server::Acceptor, InternalBindError> {
        let mut handle = self.get_mut(config, service.as_ref());

        handle.acceptor()
    }
}

#[derive(Debug)]
enum PidLock {
    Path(Utf8PathBuf),
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
pub(crate) enum ServiceHandle {
    Duplex {
        acceptor: Option<braid::server::Acceptor>,
        connector: braid::duplex::DuplexClient,
    },
    Unix {
        path: Utf8PathBuf,
        pidfile: PidLock,
    },
}

impl ServiceHandle {
    fn duplex(service: &str) -> Self {
        let (connector, acceptor) = braid::duplex::pair(service.parse().unwrap());
        Self::Duplex {
            acceptor: Some(acceptor.into()),
            connector,
        }
    }

    fn unix(path: &Utf8Path, service: &str) -> Self {
        let svcpath = path.join(service).with_extension("svc");
        let pidfile = path.join(service).with_extension("pid");

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

    async fn connect(&self, config: &RegistryConfig) -> Result<braid::client::Stream, io::Error> {
        match self {
            ServiceHandle::Duplex { connector, .. } => {
                Ok(connector.connect(config.buffer_size, None).await?.into())
            }
            ServiceHandle::Unix { path, .. } => tokio::net::UnixStream::connect(path)
                .await
                .map(|stream| stream.into()),
        }
    }

    fn acceptor(&mut self) -> Result<braid::server::Acceptor, InternalBindError> {
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
        tokio::time::timeout(*timeout, request)
            .await
            .map_err(|err| {
                tracing::warn!("failed to complete connection: {}", err);
                ConnectionError::ConnectionTimeout(name.clone().into_owned(), err)
            })?
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
        ConnectionError::ServiceClosed(name.clone().into_owned())
    })?;

    Ok(stream)
}
