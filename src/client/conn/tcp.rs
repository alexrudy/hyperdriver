//! TCP transport implementation for client connections.

use std::fmt;
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use http::Uri;
#[cfg(feature = "tls")]
use rustls::ClientConfig as TlsClientConfig;
use thiserror::Error;
use tokio::net::{TcpSocket, TcpStream};
use tokio::task::JoinError;
use tower::ServiceExt as _;
use tracing::{trace, warn, Instrument};

use super::dns::{GaiResolver, IpVersion, SocketAddrs};
use super::TransportStream;
use crate::happy_eyeballs::EyeballSet;
use crate::stream::client::Stream as ClientStream;

#[cfg(any(feature = "tls", feature = "stream"))]
use crate::stream::info::HasConnectionInfo as _;

/// A TCP connector for client connections.
#[derive(Debug, Clone)]
pub struct TcpConnector<R = GaiResolver> {
    config: Arc<TcpConnectionConfig>,
    resolver: R,

    #[cfg(feature = "tls")]
    tls: Arc<TlsClientConfig>,
}

impl TcpConnector {
    #[cfg(feature = "tls")]
    /// Create a new `TcpConnector` with the given configuration.
    pub fn new(config: TcpConnectionConfig, tls: TlsClientConfig) -> Self {
        Self {
            config: Arc::new(config),
            resolver: GaiResolver::new(),
            tls: Arc::new(tls),
        }
    }

    #[cfg(not(feature = "tls"))]
    /// Create a new `TcpConnector` with the given configuration.
    pub fn new(config: TcpConnectionConfig) -> Self {
        Self {
            config: Arc::new(config),
            resolver: GaiResolver::new(),
        }
    }
}

type BoxFuture<'a, T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

impl<R> tower::Service<Uri> for TcpConnector<R>
where
    R: tower::Service<Box<str>, Response = SocketAddrs, Error = io::Error>
        + Clone
        + Send
        + Sync
        + 'static,
    R::Future: Send,
{
    type Response = TransportStream<ClientStream<TcpStream>>;
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
            Err(e) => panic!("uri error: {e}"),
        };

        let connector = std::mem::replace(self, self.clone());

        let span = tracing::trace_span!("tcp", host = %host, port = %port);

        Box::pin(
            async move {
                let stream = connector.connect(host.clone(), port).await?;
                trace!(addr=%stream.peer_addr().expect("io error getting peer address"), "tcp connected");
                if req.scheme_str() == Some("https") {
                    #[cfg(not(feature = "tls"))]
                    {
                        panic!("TLS support is disabled");
                    }

                    #[cfg(feature = "tls")]
                    {
                        let mut stream =
                            ClientStream::new(stream).tls(host.as_ref(), connector.tls.clone());

                        stream
                            .finish_handshake()
                            .await
                            .map_err(TcpConnectionError::msg("TLS handshake"))?;

                        let info = stream.info();

                        Ok(TransportStream { stream, info })
                    }
                } else {
                    let stream = ClientStream::new(stream);

                    let info = stream.info();

                    Ok(TransportStream { stream, info })
                }
            }
            .instrument(span),
        )
    }
}

impl<R> TcpConnector<R>
where
    R: tower::Service<Box<str>, Response = SocketAddrs, Error = io::Error> + Send + Clone + 'static,
{
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
    config: &'c TcpConnectionConfig,
}

impl<'c> TcpConnecting<'c> {
    pub(crate) fn new(addresses: SocketAddrs, config: &'c TcpConnectionConfig) -> Self {
        Self { addresses, config }
    }

    #[tracing::instrument(skip(self), fields(addrs=%self.addresses.len()), level = "debug")]
    async fn connect(mut self) -> Result<TcpStream, TcpConnectionError> {
        let delay = self
            .config
            .happy_eyeballs_timeout
            .map(|duration| duration / (self.addresses.len() + 1) as u32);
        let mut attempts = EyeballSet::new(delay);

        while let Some(address) = self.addresses.pop() {
            trace!("tcp connect to {}", address);
            let attempt = TcpConnectionAttempt::new(address, self.config.clone());
            attempts.spawn(async move { attempt.connect().await });

            if attempts.len() < 2 {
                continue;
            }

            if let Some(outcome) = attempts.next().await {
                return match outcome {
                    Ok(stream) => return Ok(stream),
                    Err(e) => match e.downcast::<TcpConnectionError>() {
                        Ok(e) => Err(*e),
                        Err(e) => Err(TcpConnectionError::boxed("panic", e)),
                    },
                };
            }
        }

        attempts
            .finalize()
            .await
            .map_err(|err| match err.downcast::<TcpConnectionError>() {
                Ok(e) => *e,
                Err(e) => TcpConnectionError::boxed("panic", e),
            })
    }
}

/// Represents a single attempt to connect to a remote address.
struct TcpConnectionAttempt {
    address: SocketAddr,
    config: TcpConnectionConfig,
}

impl TcpConnectionAttempt {
    async fn connect(self) -> Result<TcpStream, TcpConnectionError> {
        let connect = connect(&self.address, self.config.connect_timeout, &self.config)?;
        connect.await
    }
}

impl TcpConnectionAttempt {
    fn new(address: SocketAddr, config: TcpConnectionConfig) -> Self {
        Self { address, config }
    }
}

/// Error type for TCP connections.
#[derive(Debug, Error)]
pub struct TcpConnectionError {
    message: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl TcpConnectionError {
    pub(super) fn new<S>(message: S) -> Self
    where
        S: Into<String>,
    {
        Self {
            message: message.into(),
            source: None,
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

    pub(super) fn boxed<S>(message: S, error: Box<dyn std::error::Error + Send + Sync>) -> Self
    where
        S: Into<String>,
    {
        Self {
            message: message.into(),
            source: Some(error),
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
pub struct TcpConnectionConfig {
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

impl Default for TcpConnectionConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Some(Duration::from_secs(10)),
            keep_alive_timeout: Some(Duration::from_secs(90)),
            happy_eyeballs_timeout: Some(Duration::from_millis(300)),
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
    let host = uri.host().ok_or(TcpConnectionError::new("missing host"))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port = match uri.port_u16() {
        Some(port) => port,
        None => match uri.scheme_str() {
            Some("http") => 80,
            Some("https") => 443,
            _ => return Err(TcpConnectionError::new("missing port")),
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
    config: &TcpConnectionConfig,
) -> Result<impl Future<Output = Result<TcpStream, TcpConnectionError>>, TcpConnectionError> {
    use socket2::{Domain, Protocol, Socket, TcpKeepalive, Type};

    let domain = Domain::for_address(*addr);
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))
        .map_err(TcpConnectionError::msg("tcp open error"))?;

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
    let connect = socket.connect(*addr);
    Ok(async move {
        match connect_timeout {
            Some(dur) => match tokio::time::timeout(dur, connect).await {
                Ok(Ok(s)) => Ok(s),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(io::Error::new(io::ErrorKind::TimedOut, e)),
            },
            None => connect.await,
        }
        .map_err(TcpConnectionError::msg("tcp connect error"))
    })
}

#[cfg(test)]
mod test {

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
}
