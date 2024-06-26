//! TCP transport implementation for client connections.
//!
//! This module contains the [`TcpTransport`] type, which is a [`tower::Service`] that connects to
//! remote addresses using TCP. It also contains the [`TcpTransportConfig`] type, which is used to
//! configure TCP connections.
//!
//! Normally, you will not need to use this module directly. Instead, you can use the [`Client`][crate::client::Client]
//! type from the [`client`][crate::client] module, which uses the [`TcpTransport`] internally by default.
//!
//! See [`Client::new_http_tcp`][crate::client::Client::new_tcp_http] for the default constructor which uses the TCP transport.

use std::fmt;
use std::future::Future;
use std::io;
use std::marker::PhantomData;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use http::Uri;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpSocket, TcpStream};
use tokio::task::JoinError;
use tower::ServiceExt as _;
use tracing::{trace, warn, Instrument};

use super::TransportStream;
use crate::client::conn::dns::{GaiResolver, IpVersion, SocketAddrs};
use crate::happy_eyeballs::{EyeballSet, HappyEyeballsError};

use crate::info::HasConnectionInfo;

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
/// # use tokio::net::TcpStream;
/// # use tower::ServiceExt as _;
///
/// # async fn run() {
/// let transport: TcpTransport<GaiResolver, TcpStream> = TcpTransport::default();
///
/// let uri = "http://example.com".parse().unwrap();
/// let stream = transport.oneshot(uri).await.unwrap();
/// # }
/// ```
#[derive(Debug)]
pub struct TcpTransport<R = GaiResolver, IO = TcpStream> {
    config: Arc<TcpTransportConfig>,
    resolver: R,
    stream: PhantomData<fn() -> IO>,
}

impl<R, IO> Clone for TcpTransport<R, IO>
where
    R: Clone,
{
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            resolver: self.resolver.clone(),
            stream: PhantomData,
        }
    }
}

impl<IO> Default for TcpTransport<GaiResolver, IO> {
    fn default() -> Self {
        TcpTransport::builder().with_gai_resolver().build()
    }
}

#[derive(Debug)]
/// Builder for a TCP connector.
pub struct TcpTransportBuilder<R> {
    config: TcpTransportConfig,
    resolver: R,
}

impl<R> TcpTransportBuilder<R> {
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

impl<R> TcpTransportBuilder<R> {
    /// Set the resolver for the TCP connector
    pub fn with_resolver<R2>(self, resolver: R2) -> TcpTransportBuilder<R2> {
        TcpTransportBuilder {
            config: self.config,
            resolver,
        }
    }

    /// Use the default GAI resolver for the TCP connector
    pub fn with_gai_resolver(self) -> TcpTransportBuilder<GaiResolver> {
        TcpTransportBuilder {
            config: self.config,
            resolver: GaiResolver::new(),
        }
    }

    /// Access the resolver for the TCP connector
    pub fn resolver(&mut self) -> &R {
        &mut self.resolver
    }
}

impl<R> TcpTransportBuilder<R>
where
    R: tower::Service<Box<str>, Response = SocketAddrs, Error = io::Error> + Send + Clone + 'static,
{
    /// Build a TCP connector with a resolver
    pub fn build<IO>(self) -> TcpTransport<R, IO> {
        TcpTransport {
            config: Arc::new(self.config),
            resolver: self.resolver,
            stream: PhantomData,
        }
    }
}

impl TcpTransport {
    /// Create a new TCP connector builder with the default configuration.
    pub fn builder() -> TcpTransportBuilder<()> {
        TcpTransportBuilder {
            config: Default::default(),
            resolver: (),
        }
    }
}

impl<R, IO> TcpTransport<R, IO> {
    /// Get the configuration for the TCP connector.
    pub fn config(&self) -> &TcpTransportConfig {
        &self.config
    }
}

type BoxFuture<'a, T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

impl<R, IO> tower::Service<Uri> for TcpTransport<R, IO>
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
{
    type Response = TransportStream<IO>;
    type Error = TcpConnectionError;
    type Future = BoxFuture<'static, Self::Response, Self::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.resolver
            .poll_ready(cx)
            .map_err(TcpConnectionError::msg("dns poll_ready"))
    }

    fn call(&mut self, req: Uri) -> Self::Future {
        let (host, port) = match get_host_and_port(&req) {
            Ok((host, port)) => (host, port),
            Err(e) => return Box::pin(std::future::ready(Err(e))),
        };

        let transport = std::mem::replace(self, self.clone());

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
                let info = stream.info();

                Ok(TransportStream {
                    stream,
                    info,
                    #[cfg(feature = "tls")]
                    tls: None,
                })
            }
            .instrument(span),
        )
    }
}

impl<R, IO> TcpTransport<R, IO>
where
    R: tower::Service<Box<str>, Response = SocketAddrs, Error = io::Error> + Send + Clone + 'static,
{
    /// Connect to a host and port.
    async fn connect(&self, host: Box<str>, port: u16) -> Result<TcpStream, TcpConnectionError> {
        let mut addrs = self
            .resolver
            .clone()
            .oneshot(host)
            .await
            .map_err(TcpConnectionError::msg("dns resolution"))?;
        addrs.set_port(port);
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

        let mut attempts = EyeballSet::new(delay, self.config.happy_eyeballs_timeout);

        while let Some(address) = self.addresses.pop() {
            let span: tracing::Span = tracing::trace_span!("connect", %address);
            let attempt = TcpConnectionAttempt::new(address, self.config);
            attempts.push(async { attempt.connect().instrument(span).await });
        }

        attempts.finish().await.map_err(|err| match err {
            HappyEyeballsError::Error(err) => err,
            HappyEyeballsError::Timeout(elapsed) => TcpConnectionError::new(format!(
                "Connection attempts timed out after {}ms",
                elapsed.as_millis()
            )),
            HappyEyeballsError::NoProgress => {
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

impl<'c> TcpConnectionAttempt<'c> {
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
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
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

/// Configuration for TCP connections.
#[derive(Debug, Clone)]
pub struct TcpTransportConfig {
    /// The timeout for connecting to a remote address.
    pub connect_timeout: Option<Duration>,

    /// The timeout for keep-alive connections.
    pub keep_alive_timeout: Option<Duration>,

    /// The timeout for happy eyeballs algorithm.
    pub happy_eyeballs_timeout: Option<Duration>,

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
            happy_eyeballs_timeout: Some(Duration::from_millis(500)),
            local_address_ipv4: None,
            local_address_ipv6: None,
            nodelay: true,
            reuse_address: true,
            send_buffer_size: None,
            recv_buffer_size: None,
        }
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

#[tracing::instrument(skip(connect_timeout, config), level = "debug")]
fn connect(
    addr: &SocketAddr,
    connect_timeout: Option<Duration>,
    config: &TcpTransportConfig,
) -> Result<impl Future<Output = Result<TcpStream, TcpConnectionError>>, TcpConnectionError> {
    use socket2::{Domain, Protocol, Socket, TcpKeepalive, Type};

    let domain = Domain::for_address(*addr);
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))
        .map_err(TcpConnectionError::msg("tcp open error"))?;
    tracing::trace!("tcp socket opened");

    let guard = tracing::trace_span!("socket_options").entered();

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

    let span = tracing::trace_span!("tcp", remote.addr = %addr);
    let connect = socket.connect(*addr).instrument(span);
    Ok(async move {
        match connect_timeout {
            Some(dur) => match tokio::time::timeout(dur, connect).await {
                Ok(Ok(s)) => Ok(s),
                Ok(Err(e)) => Err(e),
                Err(e) => {
                    tracing::trace!(timeout=?dur, "connection timed out");
                    Err(io::Error::new(io::ErrorKind::TimedOut, e))
                }
            },
            None => connect.await,
        }
        .map_err(TcpConnectionError::msg("tcp connect error"))
    })
}

#[cfg(test)]
mod test {

    use std::future::Ready;

    use tokio::net::TcpListener;
    use tower::Service;

    use crate::client::conn::Transport;

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

    async fn connect_transport<T>(
        uri: Uri,
        transport: T,
        listener: TcpListener,
    ) -> (TransportStream<T::IO>, TcpStream)
    where
        T: Transport + Service<Uri, Response = TransportStream<T::IO>>,
        <T as Service<Uri>>::Error: std::fmt::Debug,
    {
        tokio::join!(async { transport.oneshot(uri).await.unwrap() }, async {
            listener.accept().await.unwrap().0
        })
    }

    #[tokio::test]
    async fn test_tcp_invalid_uri() {
        let _ = tracing_subscriber::fmt::try_init();

        let uri: Uri = "/path/".parse().unwrap();

        let config = TcpTransportConfig::default();

        let transport = TcpTransport::builder()
            .with_config(config)
            .with_resolver(Resolver(0))
            .build::<TcpStream>();

        let result = transport.oneshot(uri).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tcp_transport() {
        let _ = tracing_subscriber::fmt::try_init();

        let bind = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = bind.local_addr().unwrap().port();

        let uri: Uri = format!("http://example.com:{port}").parse().unwrap();

        let config = TcpTransportConfig::default();

        let transport = TcpTransport::builder()
            .with_config(config)
            .with_resolver(Resolver(port))
            .build::<TcpStream>();

        let (stream, _) = connect_transport(uri, transport, bind).await;

        let info = stream.info();
        assert_eq!(
            *info.remote_addr(),
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)
        );
    }
}
