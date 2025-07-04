//! TCP transport implementation for client connections.
//!
//! This module contains the [`TcpTransport`] type, which is a [`tower::Service`] that connects to
//! remote addresses using TCP. It also contains the [`TcpTransportConfig`] type, which is used to
//! configure TCP connections.
//!
//! Normally, you will not need to use this module directly. Instead, you can use the [`Client`][crate::client::Client]
//! type from the [`client`][crate::client] module, which uses the [`TcpTransport`] internally by default.
//!
//! See [`Client::build_tcp_http`][crate::client::Client::build_tcp_http] for the default constructor which uses the TCP transport.

use std::fmt;
use std::future::Future;
use std::io;
use std::marker::PhantomData;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::future::FutureExt as _;
use http::Uri;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpSocket;
use tokio::task::JoinError;
use tracing::{trace, warn, Instrument};

use crate::client::builder::BuildTransport;
use crate::client::conn::dns::{GaiResolver, IpVersion, ResolverExt as _, SocketAddrs};
use crate::happy_eyeballs::{EyeballSet, HappyEyeballsError};
use crate::info::HasConnectionInfo;
use crate::stream::tcp::TcpStream;
use crate::BoxError;

/// A TCP connector for client connections.
///
/// This type is a [`tower::Service`] that connects to remote addresses using TCP.
/// It requires a resolver `R`, which is by default [`GaiResolver`], which uses
/// the system's DNS resolver to resolve hostnames to IP addresses.
///
/// The connector can be configured with a [`TcpTransportConfig`] to control
/// various aspects of the TCP connection, such as timeouts, buffer sizes, and
/// local addresses to bind to.
///
#[cfg_attr(
    feature = "tls",
    doc = "If the `tls` feature is enabled, the connector can also be configured with
    a [`rustls::ClientConfig`] to enable TLS support"
)]
///
/// This connector implements the happy-eyeballs algorithm for connecting to
/// remote addresses, which allows for faster connection times by trying
/// multiple addresses in parallel, regardless of whether they are IPv4 or IPv6.
///
/// # Example
/// ```no_run
/// # use hyperdriver::client::conn::transport::tcp::TcpTransport;
/// # use hyperdriver::client::conn::dns::GaiResolver;
/// # use hyperdriver::stream::tcp::TcpStream;
/// # use hyperdriver::IntoRequestParts;
/// # use tower::ServiceExt as _;
///
/// # async fn run() {
/// let transport: TcpTransport<GaiResolver, TcpStream> = TcpTransport::default();
///
/// let uri = "http://example.com".into_request_parts();
/// let stream = transport.oneshot(uri).await.unwrap();
/// # }
/// ```
#[derive(Debug)]
pub struct TcpTransport<R = GaiResolver, IO = TcpStream, H = HostFromUri> {
    config: Arc<TcpTransportConfig>,
    resolver: R,
    gethost: H,
    stream: PhantomData<fn() -> IO>,
}

impl<R, IO, H> Clone for TcpTransport<R, IO, H>
where
    R: Clone,
    H: Clone,
{
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            resolver: self.resolver.clone(),
            gethost: self.gethost.clone(),
            stream: PhantomData,
        }
    }
}

impl<IO> Default for TcpTransport<GaiResolver, IO, HostFromUri> {
    fn default() -> Self {
        TcpTransport::builder()
            .with_gai_resolver()
            .with_host_from_uri()
            .build()
    }
}

#[derive(Debug)]
/// Builder for a TCP connector.
pub struct TcpTransportBuilder<R, H> {
    config: TcpTransportConfig,
    resolver: R,
    gethost: H,
}

impl<R, H> TcpTransportBuilder<R, H> {
    /// Access the TCP connection configuration
    pub fn config(&mut self) -> &mut TcpTransportConfig {
        &mut self.config
    }

    /// Set the TCP connection configuration
    pub fn with_config(mut self, config: TcpTransportConfig) -> Self {
        self.config = config;
        self
    }
}

impl<R, H> TcpTransportBuilder<R, H> {
    /// Set the resolver for the TCP connector
    pub fn with_resolver<R2>(self, resolver: R2) -> TcpTransportBuilder<R2, H> {
        TcpTransportBuilder {
            config: self.config,
            resolver,
            gethost: self.gethost,
        }
    }

    /// Use the default GAI resolver for the TCP connector
    pub fn with_gai_resolver(self) -> TcpTransportBuilder<GaiResolver, H> {
        TcpTransportBuilder {
            config: self.config,
            resolver: GaiResolver::new(),
            gethost: self.gethost,
        }
    }

    /// Access the resolver for the TCP connector
    pub fn resolver(&mut self) -> &R {
        &mut self.resolver
    }
}

impl<R, H> TcpTransportBuilder<R, H> {
    /// Parse the host and port from the Uri in the request data
    pub fn with_host_from_uri(self) -> TcpTransportBuilder<R, HostFromUri> {
        TcpTransportBuilder {
            config: self.config,
            resolver: self.resolver,
            gethost: HostFromUri::default(),
        }
    }

    /// Set a way to get the host from request data.
    pub fn with_host_from<H2>(self, get_host: H2) -> TcpTransportBuilder<R, H2> {
        TcpTransportBuilder {
            config: self.config,
            resolver: self.resolver,
            gethost: get_host,
        }
    }
}

impl<R, H> TcpTransportBuilder<R, H>
where
    R: tower::Service<Box<str>, Response = SocketAddrs, Error = io::Error> + Send + Clone + 'static,
    H: GetHostAndPort,
{
    /// Build a TCP connector with a resolver
    pub fn build<IO>(self) -> TcpTransport<R, IO, H> {
        TcpTransport {
            config: Arc::new(self.config),
            resolver: self.resolver,
            gethost: self.gethost,
            stream: PhantomData,
        }
    }
}

impl TcpTransport {
    /// Create a new TCP connector builder with the default configuration.
    pub fn builder() -> TcpTransportBuilder<(), ()> {
        TcpTransportBuilder {
            config: Default::default(),
            resolver: (),
            gethost: (),
        }
    }
}

impl<R, IO> TcpTransport<R, IO> {
    /// Get the configuration for the TCP connector.
    pub fn config(&self) -> &TcpTransportConfig {
        &self.config
    }
}

type BoxFuture<'a, T, E> = crate::BoxFuture<'a, Result<T, E>>;

impl<R, IO, H> tower::Service<http::request::Parts> for TcpTransport<R, IO, H>
where
    R: tower::Service<Box<str>, Response = SocketAddrs, Error = io::Error>
        + Clone
        + Send
        + Sync
        + 'static,
    R::Future: Send,
    TcpStream: Into<IO>,
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    IO::Addr: Clone + Unpin + Send + 'static,
    H: GetHostAndPort + Clone + Send + 'static,
{
    type Response = IO;
    type Error = TcpConnectionError;
    type Future = BoxFuture<'static, Self::Response, Self::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.resolver
            .poll_ready(cx)
            .map_err(TcpConnectionError::msg("dns poll_ready"))
    }

    fn call(&mut self, req: http::request::Parts) -> Self::Future {
        let (host, port) = match self.gethost.get_host_and_port(&req) {
            Ok((host, port)) => (host, port),
            Err(e) => return Box::pin(std::future::ready(Err(e))),
        };

        let mut transport = std::mem::replace(self, self.clone());

        let span = tracing::trace_span!("tcp", host = %host, port = %port);

        Box::pin(
            async move {
                let stream = transport.connect(host.clone(), port).await?;

                if let Ok(peer_addr) = stream.peer_addr() {
                    trace!(peer.addr = %peer_addr, "tcp connected");
                } else {
                    trace!("tcp connected");
                }

                let stream = stream.into();

                Ok(stream)
            }
            .instrument(span),
        )
    }
}

impl<R, IO, H> TcpTransport<R, IO, H>
where
    R: tower::Service<Box<str>, Response = SocketAddrs, Error = io::Error> + Send + Clone + 'static,
    R::Future: Send + 'static,
{
    async fn resolve(&mut self, host: Box<str>) -> Result<SocketAddrs, TcpConnectionError> {
        let resolver = R::clone(&self.resolver);
        std::mem::replace(&mut self.resolver, resolver)
            .resolve(host, self.config.connect_timeout)
            .await
            .map_err(TcpConnectionError::msg("dns resolve error"))
    }

    /// Connect to a host and port.
    async fn connect(
        &mut self,
        host: Box<str>,
        port: u16,
    ) -> Result<TcpStream, TcpConnectionError> {
        let mut addrs = self.resolve(host).await?;

        addrs.set_port(port);
        tracing::trace!("dns resolution finished");
        let connecting = self.connecting(addrs);
        connecting.connect().await
    }

    /// Create a new `TcpConnecting` future.
    fn connecting(&self, mut addrs: SocketAddrs) -> TcpConnecting<'_> {
        if self.config.happy_eyeballs_timeout.is_some() {
            addrs.sort_preferred(IpVersion::from_binding(
                self.config.local_address_ipv4,
                self.config.local_address_ipv6,
            ));
        }

        TcpConnecting::new(addrs, &self.config)
    }

    /// Connect to a set of remote addresses using the transport's configured settings and happy eyeballs algorithm.
    ///
    /// This is useful when you have a set of addresses to connect to, and you want to use the happy eyeballs algorithm
    /// but you don't want to do address resolution again (or you have a static list of addresses to try).
    pub async fn connect_to_addrs<A>(&self, addrs: A) -> Result<TcpStream, TcpConnectionError>
    where
        A: IntoIterator<Item = SocketAddr>,
    {
        let addrs = SocketAddrs::from_iter(addrs);
        let connecting = self.connecting(addrs);
        connecting.connect().await
    }
}

/// Establish a TCP connection to a set of addresses with a given config.
///
/// This is a low-level method which allows for library-level re-use of things like the happy-eyeballs algorithm
/// and connection attempt management.
pub async fn connect_to_addrs<A>(
    config: &TcpTransportConfig,
    addrs: A,
) -> Result<TcpStream, TcpConnectionError>
where
    A: IntoIterator<Item = SocketAddr>,
{
    let mut addrs = SocketAddrs::from_iter(addrs);
    if config.happy_eyeballs_timeout.is_some() {
        addrs.sort_preferred(IpVersion::from_binding(
            config.local_address_ipv4,
            config.local_address_ipv6,
        ));
    }

    let connecting = TcpConnecting::new(addrs, config);
    connecting.connect().await
}

/// Future which implements the happy eyeballs algorithm for connecting to a remote address.
///
/// This follows the algorithm described in [RFC8305](https://tools.ietf.org/html/rfc8305),
/// which allows for faster connection times by trying multiple addresses in parallel,
/// regardless of whether they are IPv4 or IPv6.
pub(crate) struct TcpConnecting<'c> {
    addresses: SocketAddrs,
    config: &'c TcpTransportConfig,
}

impl<'c> TcpConnecting<'c> {
    /// Create a new `TcpConnecting` future.
    pub(crate) fn new(addresses: SocketAddrs, config: &'c TcpTransportConfig) -> Self {
        Self { addresses, config }
    }

    /// Connect to the remote address using the happy eyeballs algorithm.
    async fn connect(mut self) -> Result<TcpStream, TcpConnectionError> {
        let delay = if self.addresses.is_empty() {
            self.config.happy_eyeballs_timeout
        } else {
            self.config
                .happy_eyeballs_timeout
                .map(|duration| duration / (self.addresses.len()) as u32)
        };

        tracing::trace!(?delay, timeout=?self.config.happy_eyeballs_timeout, "happy eyeballs");
        let mut attempts = EyeballSet::new(
            delay,
            self.config.happy_eyeballs_timeout,
            self.config.happy_eyeballs_concurrency,
        );

        while let Some(address) = self.addresses.pop() {
            let span: tracing::Span = tracing::trace_span!("connect", %address);
            let attempt = TcpConnectionAttempt::new(address, self.config);
            attempts.push(async { attempt.connect().instrument(span).await });
        }

        tracing::trace!("Starting {} connection attempts", attempts.len());

        attempts.finish().await.map_err(|err| match err {
            HappyEyeballsError::Error(err) => err,
            HappyEyeballsError::Timeout(elapsed) => {
                tracing::trace!("tcp timed out after {}ms", elapsed.as_millis());
                TcpConnectionError::new(format!(
                    "Connection attempts timed out after {}ms",
                    elapsed.as_millis()
                ))
            }
            HappyEyeballsError::NoProgress => {
                tracing::trace!("tcp exhausted connection candidates");
                TcpConnectionError::new("Exhausted connection candidates")
            }
        })
    }
}

/// Represents a single attempt to connect to a remote address.
///
/// This exists to allow us to move the SocketAddr but borrow the TcpTransportConfig.
struct TcpConnectionAttempt<'c> {
    address: SocketAddr,
    config: &'c TcpTransportConfig,
}

impl TcpConnectionAttempt<'_> {
    /// Make a single connection attempt.
    async fn connect(self) -> Result<TcpStream, TcpConnectionError> {
        let connect = connect(&self.address, self.config.connect_timeout, self.config)?;
        connect.await
    }
}

impl<'c> TcpConnectionAttempt<'c> {
    fn new(address: SocketAddr, config: &'c TcpTransportConfig) -> Self {
        Self { address, config }
    }
}

/// Error type for invalid URIs during connection.
#[derive(Debug, Error)]
#[error("invalid URI")]
pub struct InvalidUri {
    _priv: (),
}

/// Error type for TCP connections.
#[derive(Debug, Error)]
pub struct TcpConnectionError {
    message: String,
    #[source]
    source: Option<BoxError>,
}

impl TcpConnectionError {
    #[allow(dead_code)]
    pub(super) fn new<S>(message: S) -> Self
    where
        S: Into<String>,
    {
        Self {
            message: message.into(),
            source: None,
        }
    }

    fn uri<S>(message: S) -> Self
    where
        S: Into<String>,
    {
        Self {
            message: message.into(),
            source: Some(InvalidUri { _priv: () }.into()),
        }
    }

    pub(super) fn msg<S, E>(message: S) -> impl FnOnce(E) -> Self
    where
        S: Into<String>,
        E: std::error::Error + Send + Sync + 'static,
    {
        move |error| Self {
            message: message.into(),
            source: Some(error.into()),
        }
    }

    pub(super) fn build<S, E>(message: S, error: E) -> Self
    where
        S: Into<String>,
        E: std::error::Error + Send + Sync + 'static,
    {
        Self {
            message: message.into(),
            source: Some(Box::new(error)),
        }
    }
}

impl From<JoinError> for TcpConnectionError {
    fn from(value: JoinError) -> Self {
        Self::build("tcp connection panic", value)
    }
}

impl fmt::Display for TcpConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ref source) = self.source {
            write!(f, "{}: {}", self.message, source)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl From<TcpConnectionError> for io::Error {
    fn from(value: TcpConnectionError) -> Self {
        if let Some(original) = value
            .source
            .as_ref()
            .and_then(|r| r.downcast_ref::<io::Error>())
        {
            io::Error::new(original.kind(), original.to_string())
        } else {
            io::Error::other(value)
        }
    }
}

/// Configuration for TCP connections.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TcpTransportConfig {
    /// The timeout for connecting to a remote address.
    pub connect_timeout: Option<Duration>,

    /// The timeout for keep-alive connections.
    pub keep_alive_timeout: Option<Duration>,

    /// The timeout for happy eyeballs algorithm.
    pub happy_eyeballs_timeout: Option<Duration>,

    /// The number of concurrent connection atttempts to make.
    pub happy_eyeballs_concurrency: Option<usize>,

    /// The local IPv4 address to bind to.
    pub local_address_ipv4: Option<Ipv4Addr>,

    /// The local IPv6 address to bind to.
    pub local_address_ipv6: Option<Ipv6Addr>,

    /// Whether to disable Nagle's algorithm.
    pub nodelay: bool,

    /// Whether to reuse the local address.
    pub reuse_address: bool,

    /// The size of the send buffer.
    pub send_buffer_size: Option<usize>,

    /// The size of the receive buffer.
    pub recv_buffer_size: Option<usize>,
}

impl Default for TcpTransportConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Some(Duration::from_secs(10)),
            keep_alive_timeout: Some(Duration::from_secs(90)),
            happy_eyeballs_timeout: Some(Duration::from_secs(30)),
            happy_eyeballs_concurrency: Some(2),
            local_address_ipv4: None,
            local_address_ipv6: None,
            nodelay: true,
            reuse_address: true,
            send_buffer_size: None,
            recv_buffer_size: None,
        }
    }
}

impl BuildTransport for TcpTransportConfig {
    type Target = TcpTransport;

    fn build(self) -> Self::Target {
        TcpTransport::builder()
            .with_config(self)
            .with_gai_resolver()
            .with_host_from_uri()
            .build()
    }
}

/// A simple TCP transport that uses a single connection attempt.
pub struct SimpleTcpTransport<R, IO = TcpStream> {
    config: Arc<TcpTransportConfig>,
    resolver: R,
    stream: PhantomData<fn() -> IO>,
}

impl<R, IO> fmt::Debug for SimpleTcpTransport<R, IO> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SimpleTcpTransport").finish()
    }
}

impl<R: Clone, IO> Clone for SimpleTcpTransport<R, IO> {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            resolver: self.resolver.clone(),
            stream: PhantomData,
        }
    }
}

impl<R, IO> SimpleTcpTransport<R, IO> {
    /// Create a new simple TCP transport with the given configuration and resolver.
    pub fn new(config: TcpTransportConfig, resolver: R) -> Self {
        Self {
            config: Arc::new(config),
            resolver,
            stream: PhantomData,
        }
    }

    /// Chagne the resolver for the transport.
    pub fn with_resolver<R2>(self, resolver: R2) -> SimpleTcpTransport<R2, IO> {
        SimpleTcpTransport {
            config: self.config,
            resolver,
            stream: PhantomData,
        }
    }
}

impl<R, IO> SimpleTcpTransport<R, IO>
where
    R: tower::Service<Box<str>, Response = IpAddr, Error = io::Error> + Send + Clone + 'static,
    R::Future: Send + 'static,
{
    async fn resolve(&mut self, host: Box<str>) -> Result<IpAddr, TcpConnectionError> {
        let resolver = R::clone(&self.resolver);
        std::mem::replace(&mut self.resolver, resolver)
            .resolve(host, self.config.connect_timeout)
            .await
            .map_err(TcpConnectionError::msg("dns resolve error"))
    }

    async fn connect(
        &mut self,
        host: Box<str>,
        port: u16,
    ) -> Result<TcpStream, TcpConnectionError> {
        let ipaddr = self.resolve(host).await?;
        let addr = SocketAddr::new(ipaddr, port);
        trace!("tcp connecting to {}", addr);

        self.connect_to_addr(addr).await
    }

    async fn connect_to_addr(&self, addr: SocketAddr) -> Result<TcpStream, TcpConnectionError> {
        let connect = connect(&addr, self.config.connect_timeout, &self.config)?;
        connect
            .await
            .map_err(TcpConnectionError::msg("tcp connect error"))
    }
}

impl<R, IO> tower::Service<http::request::Parts> for SimpleTcpTransport<R, IO>
where
    R: tower::Service<Box<str>, Response = IpAddr, Error = io::Error>
        + Clone
        + Send
        + Sync
        + 'static,
    R::Future: Send + 'static,
    TcpStream: Into<IO>,
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    IO::Addr: Clone + Unpin + Send + 'static,
{
    type Response = IO;
    type Error = TcpConnectionError;
    type Future = BoxFuture<'static, Self::Response, Self::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.resolver
            .poll_ready(cx)
            .map_err(TcpConnectionError::msg("dns poll_ready"))
    }

    fn call(&mut self, req: http::request::Parts) -> Self::Future {
        let (host, port) = match get_host_and_port(&req.uri) {
            Ok((host, port)) => (host, port),
            Err(e) => return Box::pin(std::future::ready(Err(e))),
        };

        let mut transport = std::mem::replace(self, self.clone());

        let span = tracing::trace_span!("tcp", host = %host, port = %port);

        async move {
            let stream = transport.connect(host, port).await?;

            if let Ok(peer_addr) = stream.peer_addr() {
                trace!(peer.addr = %peer_addr, "tcp connected");
            } else {
                trace!("tcp connected");
                trace!("no peer address available");
            }

            let stream = stream.into();

            Ok(stream)
        }
        .instrument(span)
        .boxed()
    }
}

/// Get the host and port by parsing them from the URI,
/// and using the scheme to infer the port if not present.
#[derive(Debug, Default, Clone)]
pub struct HostFromUri {
    _priv: (),
}

/// Trait for producing the host and port from a request.
pub trait GetHostAndPort {
    /// Parse information from the request to identify
    /// the host and URI. Most often, [`HostFromUri`] is
    /// the right default.
    fn get_host_and_port(
        &self,
        req: &http::request::Parts,
    ) -> Result<(Box<str>, u16), TcpConnectionError>;
}

impl GetHostAndPort for HostFromUri {
    fn get_host_and_port(
        &self,
        req: &http::request::Parts,
    ) -> Result<(Box<str>, u16), TcpConnectionError> {
        self::get_host_and_port(&req.uri)
    }
}

fn get_host_and_port(uri: &Uri) -> Result<(Box<str>, u16), TcpConnectionError> {
    let host = uri.host().ok_or(TcpConnectionError::uri("missing host"))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port = match uri.port_u16() {
        Some(port) => port,
        None => match uri.scheme_str() {
            Some("http") => 80,
            Some("https") => 443,
            _ => return Err(TcpConnectionError::uri("missing port")),
        },
    };

    Ok((host.into(), port))
}

fn bind_local_address(
    socket: &socket2::Socket,
    dst_addr: &SocketAddr,
    local_addr_ipv4: &Option<Ipv4Addr>,
    local_addr_ipv6: &Option<Ipv6Addr>,
) -> io::Result<()> {
    match (*dst_addr, local_addr_ipv4, local_addr_ipv6) {
        (SocketAddr::V4(_), Some(addr), _) => {
            socket.bind(&SocketAddr::new((*addr).into(), 0).into())?;
        }
        (SocketAddr::V6(_), _, Some(addr)) => {
            socket.bind(&SocketAddr::new((*addr).into(), 0).into())?;
        }
        _ => {}
    }

    Ok(())
}

pub(crate) fn connect(
    addr: &SocketAddr,
    connect_timeout: Option<Duration>,
    config: &TcpTransportConfig,
) -> Result<impl Future<Output = Result<TcpStream, TcpConnectionError>>, TcpConnectionError> {
    use socket2::{Domain, Protocol, Socket, TcpKeepalive, Type};

    let domain = Domain::for_address(*addr);
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))
        .map_err(TcpConnectionError::msg("tcp open error"))?;
    tracing::trace!("tcp socket opened");

    let guard = tracing::trace_span!("socket::options").entered();

    // When constructing a Tokio `TcpSocket` from a raw fd/socket, the user is
    // responsible for ensuring O_NONBLOCK is set.
    socket
        .set_nonblocking(true)
        .map_err(TcpConnectionError::msg("tcp set_nonblocking error"))?;

    if let Some(dur) = config.keep_alive_timeout {
        let conf = TcpKeepalive::new().with_time(dur);
        if let Err(e) = socket.set_tcp_keepalive(&conf) {
            warn!("tcp set_keepalive error: {}", e);
        }
    }

    bind_local_address(
        &socket,
        addr,
        &config.local_address_ipv4,
        &config.local_address_ipv6,
    )
    .map_err(TcpConnectionError::msg("tcp bind local address"))?;

    #[allow(unsafe_code)]
    let socket = unsafe {
        // Safety: `from_raw_fd` is only safe to call if ownership of the raw
        // file descriptor is transferred. Since we call `into_raw_fd` on the
        // socket2 socket, it gives up ownership of the fd and will not close
        // it, so this is safe.
        use std::os::unix::io::{FromRawFd, IntoRawFd};
        TcpSocket::from_raw_fd(socket.into_raw_fd())
    };

    if config.reuse_address {
        if let Err(e) = socket.set_reuseaddr(true) {
            warn!("tcp set_reuse_address error: {}", e);
        }
    }

    if let Some(size) = config.send_buffer_size {
        if let Err(e) = socket.set_send_buffer_size(size.try_into().unwrap_or(u32::MAX)) {
            warn!("tcp set_buffer_size error: {}", e);
        }
    }

    if let Some(size) = config.recv_buffer_size {
        if let Err(e) = socket.set_recv_buffer_size(size.try_into().unwrap_or(u32::MAX)) {
            warn!("tcp set_recv_buffer_size error: {}", e);
        }
    }

    drop(guard);

    let span = tracing::trace_span!("socket::connect", remote.addr = %addr);
    let connect = socket.connect(*addr).instrument(span);
    Ok(async move {
        match connect_timeout {
            Some(dur) => match tokio::time::timeout(dur, connect).await {
                Ok(Ok(s)) => Ok(TcpStream::client(s)),
                Ok(Err(e)) => Err(e),
                Err(e) => {
                    tracing::trace!(timeout=?dur, "connection timed out");
                    Err(io::Error::new(io::ErrorKind::TimedOut, e))
                }
            },
            None => connect.await.map(TcpStream::client),
        }
        .map_err(TcpConnectionError::msg("tcp connect error"))
    })
}

#[cfg(test)]
mod test {

    use std::future::Ready;

    use tokio::net::TcpListener;
    use tower::{Service, ServiceExt as _};

    use crate::{
        client::conn::{dns::FirstAddrResolver, Transport},
        IntoRequestParts,
    };

    use super::*;

    #[test]
    fn test_get_host_and_port() {
        let uri: Uri = "http://example.com".parse().unwrap();
        assert_eq!(get_host_and_port(&uri).unwrap(), ("example.com".into(), 80));

        let uri: Uri = "http://example.com:8080".parse().unwrap();
        assert_eq!(
            get_host_and_port(&uri).unwrap(),
            ("example.com".into(), 8080)
        );

        let uri: Uri = "https://example.com".parse().unwrap();
        assert_eq!(
            get_host_and_port(&uri).unwrap(),
            ("example.com".into(), 443)
        );

        let uri: Uri = "https://example.com:8443".parse().unwrap();
        assert_eq!(
            get_host_and_port(&uri).unwrap(),
            ("example.com".into(), 8443)
        );

        let uri: Uri = "grpc://example.com".parse().unwrap();
        assert!(get_host_and_port(&uri).is_err());

        let uri: Uri = "grpc://[::1]".parse().unwrap();
        assert!(get_host_and_port(&uri).is_err());
    }

    /// Always resolves to localhost at the given port.
    #[derive(Debug, Clone)]
    struct Resolver(u16);

    impl Service<Box<str>> for Resolver {
        type Response = SocketAddrs;
        type Error = io::Error;
        type Future = Ready<Result<SocketAddrs, io::Error>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Box<str>) -> Self::Future {
            std::future::ready(Ok(SocketAddrs::from_iter(vec![SocketAddr::new(
                Ipv4Addr::LOCALHOST.into(),
                self.0,
            )])))
        }
    }

    #[derive(Debug, Clone)]
    struct EmptyResolver;

    impl Service<Box<str>> for EmptyResolver {
        type Response = SocketAddrs;
        type Error = io::Error;
        type Future = Ready<Result<SocketAddrs, io::Error>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Box<str>) -> Self::Future {
            std::future::ready(Ok::<_, io::Error>(SocketAddrs::default()))
        }
    }

    #[derive(Debug, Clone)]
    struct ErrorResolver;

    impl Service<Box<str>> for ErrorResolver {
        type Response = SocketAddrs;
        type Error = io::Error;
        type Future = Ready<Result<SocketAddrs, io::Error>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Box<str>) -> Self::Future {
            std::future::ready(Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "no address found",
            )))
        }
    }

    async fn connect_transport<T>(
        uri: Uri,
        transport: T,
        listener: TcpListener,
    ) -> (T::IO, TcpStream)
    where
        T: Transport + Service<http::request::Parts, Response = T::IO>,
        <T as Service<http::request::Parts>>::Error: std::fmt::Debug,
    {
        tokio::join!(
            async { transport.oneshot(uri.into_request_parts()).await.unwrap() },
            async {
                let (stream, addr) = listener.accept().await.unwrap();
                TcpStream::server(stream, addr)
            }
        )
    }

    #[tokio::test]
    async fn test_tcp_invalid_uri() {
        let _ = tracing_subscriber::fmt::try_init();

        let uri: Uri = "/path/".parse().unwrap();

        let config = TcpTransportConfig::default();

        let transport = TcpTransport::builder()
            .with_config(config)
            .with_resolver(Resolver(0))
            .with_host_from_uri()
            .build::<TcpStream>();

        let result = transport.oneshot(uri.into_request_parts()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_transport() {
        let _ = tracing_subscriber::fmt::try_init();

        let bind = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = bind.local_addr().unwrap().port();

        let uri: Uri = format!("http://example.com:{port}").parse().unwrap();

        let config = TcpTransportConfig::default();

        let transport = TcpTransport::builder()
            .with_config(config)
            .with_resolver(Resolver(port))
            .with_host_from_uri()
            .build::<TcpStream>();

        let (stream, _) = connect_transport(uri, transport, bind).await;

        let info = stream.info();
        assert_eq!(
            *info.remote_addr(),
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)
        );
    }

    #[tokio::test]
    async fn test_transport_no_candidates() {
        let _ = tracing_subscriber::fmt::try_init();

        let uri: Uri = "http://example.com".parse().unwrap();

        let config = TcpTransportConfig::default();

        let transport = TcpTransport::builder()
            .with_config(config)
            .with_resolver(EmptyResolver)
            .with_host_from_uri()
            .build::<TcpStream>();

        let result = transport.oneshot(uri.into_request_parts()).await;
        assert!(result.is_err());

        let err = result.unwrap_err();

        assert!(!err.to_string().contains("timed out"))
    }

    #[tokio::test]
    async fn test_transport_error() {
        let _ = tracing_subscriber::fmt::try_init();

        let parts = "http://example.com".into_request_parts();

        let config = TcpTransportConfig::default();

        let transport = TcpTransport::builder()
            .with_config(config)
            .with_resolver(ErrorResolver)
            .with_host_from_uri()
            .build::<TcpStream>();

        let result = transport.oneshot(parts).await;
        assert!(result.is_err());

        let err = result.unwrap_err();

        assert!(err.to_string().contains("no address found"))
    }

    #[tokio::test]
    async fn test_transport_connect_to_addrs() {
        let _ = tracing_subscriber::fmt::try_init();

        let bind = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = bind.local_addr().unwrap().port();

        let config = TcpTransportConfig::default();
        let transport = TcpTransport::builder()
            .with_config(config)
            .with_resolver(Resolver(port))
            .with_host_from_uri()
            .build::<TcpStream>();

        let addrs = vec![
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port),
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port + 1),
        ];

        let (conn, _) = tokio::join!(
            async { transport.connect_to_addrs(addrs).await.unwrap() },
            async {
                let (stream, addr) = bind.accept().await.unwrap();
                TcpStream::server(stream, addr)
            }
        );

        let info = conn.info();
        assert_eq!(
            *info.remote_addr(),
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)
        );
    }

    #[tokio::test]
    async fn test_simple_transport() {
        let _ = tracing_subscriber::fmt::try_init();

        let bind = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = bind.local_addr().unwrap().port();

        let config = TcpTransportConfig::default();
        let transport: SimpleTcpTransport<_, TcpStream> =
            SimpleTcpTransport::new(config, FirstAddrResolver::new(Resolver(port)));

        let uri: Uri = format!("http://example.com:{port}").parse().unwrap();
        let (conn, _) = connect_transport(uri, transport, bind).await;

        let info = conn.info();
        assert_eq!(
            *info.remote_addr(),
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)
        );
    }
}
