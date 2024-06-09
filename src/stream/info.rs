//! Connection info for braid streams.

use std::fmt;
use std::io;
use std::str::FromStr;

#[cfg(feature = "stream")]
use camino::Utf8Path;
use camino::Utf8PathBuf;
use http::uri::Authority;
use thiserror::Error;
use tokio::net::{TcpStream, UnixStream};

#[cfg(not(feature = "stream"))]
use crate::stream::duplex::DuplexAddr;
#[cfg(feature = "tls")]
use crate::stream::tls::info::TlsConnectionInfo;

/// The transport protocol used for a connection.
///
/// This is for informational purposes only, and can be used
/// to select the appropriate transport when a transport should
/// be pre-negotiated (e.g. ALPN or a Duplex socket).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Protocol {
    /// HTTP transports
    Http(http::Version),

    /// gRPC
    Grpc,

    /// WebSocket
    WebSocket,

    /// Other protocol
    Other(String),
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // http::Version uses the debug format to write out the version
            Self::Http(version) => write!(f, "{:?}", version),
            Self::Grpc => write!(f, "gRPC"),
            Self::WebSocket => write!(f, "WebSocket"),
            Self::Other(s) => write!(f, "{}", s),
        }
    }
}

impl Protocol {
    /// Create a new protocol with the given http version.
    pub fn http(version: http::Version) -> Self {
        Self::Http(version)
    }

    /// New gRPC protocol
    pub fn grpc() -> Self {
        Self::Grpc
    }

    /// New WebSocket protocol
    pub fn web_socket() -> Self {
        Self::WebSocket
    }
}

impl From<http::Version> for Protocol {
    fn from(version: http::Version) -> Self {
        Self::Http(version)
    }
}

/// Error returned when a protocol is invalid.
#[derive(Debug, Error)]
#[error("invalid protocol")]
pub struct InvalidProtocol;

impl FromStr for Protocol {
    type Err = InvalidProtocol;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "http/0.9" => Ok(Self::Http(http::Version::HTTP_09)),
            "http/1.0" => Ok(Self::Http(http::Version::HTTP_10)),
            "http/1.1" => Ok(Self::Http(http::Version::HTTP_11)),
            "h2" => Ok(Self::Http(http::Version::HTTP_2)),
            "h3" => Ok(Self::Http(http::Version::HTTP_3)),
            "gRPC" => Ok(Self::Grpc),
            "WebSocket" => Ok(Self::WebSocket),
            _ => Ok(Self::Other(s.to_string())),
        }
    }
}

/// Canonicalize a socket address, converting IPv4 addresses which are
/// mapped into IPv6 addresses into standard IPv4 addresses.
#[cfg(feature = "stream")]
fn make_canonical(addr: std::net::SocketAddr) -> std::net::SocketAddr {
    match addr.ip() {
        std::net::IpAddr::V4(_) => addr,
        std::net::IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                std::net::SocketAddr::new(std::net::IpAddr::V4(ip), addr.port())
            } else {
                addr
            }
        }
    }
}

/// A socket address for a Braid stream.
///
/// Supports more than just network socket addresses, also support Unix socket addresses (paths)
/// and unnamed Duplex and Unix socket connections.
#[cfg(feature = "stream")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BraidAddr {
    /// A TCP socket address.
    Tcp(std::net::SocketAddr),

    /// Represents a duplex connection which has no address.
    Duplex,

    /// A Unix socket address.
    Unix(Utf8PathBuf),

    /// Represents a Unix socket connection which has no address.
    UnixUnnamed,
}

#[cfg(feature = "stream")]
impl std::fmt::Display for BraidAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp(addr) => write!(f, "{}", addr),
            Self::Duplex => write!(f, "<duplex>"),
            Self::Unix(path) => write!(f, "{}", path),
            Self::UnixUnnamed => write!(f, "<unnamed>"),
        }
    }
}

#[cfg(feature = "stream")]
impl BraidAddr {
    /// Returns the TCP socket address, if this is a TCP socket address.
    pub fn tcp(&self) -> Option<std::net::SocketAddr> {
        match self {
            Self::Tcp(addr) => Some(*addr),
            _ => None,
        }
    }

    /// Returns the Unix socket address, if this is a Unix socket address.
    pub fn path(&self) -> Option<&Utf8Path> {
        match self {
            Self::Unix(path) => Some(path.as_path()),
            _ => None,
        }
    }

    /// Returns the canonical TCP address, if this is a TCP socket address.
    pub fn canonical(self) -> Self {
        match self {
            Self::Tcp(addr) => Self::Tcp(make_canonical(addr)),
            _ => self,
        }
    }
}

#[cfg(feature = "stream")]
impl From<std::net::SocketAddr> for BraidAddr {
    fn from(addr: std::net::SocketAddr) -> Self {
        Self::Tcp(make_canonical(addr))
    }
}

#[cfg(feature = "stream")]
impl TryFrom<tokio::net::unix::SocketAddr> for BraidAddr {
    type Error = io::Error;
    fn try_from(addr: tokio::net::unix::SocketAddr) -> Result<Self, Self::Error> {
        let path = match addr.as_pathname() {
            Some(path) => path.to_path_buf(),
            None => {
                return Ok(Self::UnixUnnamed);
            }
        };

        let path = Utf8PathBuf::from_path_buf(path).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid (non-utf8) unix socket address",
            )
        })?;

        Ok(Self::Unix(path))
    }
}

#[cfg(feature = "stream")]
impl From<(std::net::IpAddr, u16)> for BraidAddr {
    fn from(addr: (std::net::IpAddr, u16)) -> Self {
        Self::Tcp(std::net::SocketAddr::new(addr.0, addr.1))
    }
}

#[cfg(feature = "stream")]
impl From<(std::net::Ipv4Addr, u16)> for BraidAddr {
    fn from(addr: (std::net::Ipv4Addr, u16)) -> Self {
        Self::Tcp(std::net::SocketAddr::new(
            std::net::IpAddr::V4(addr.0),
            addr.1,
        ))
    }
}

#[cfg(feature = "stream")]
impl From<(std::net::Ipv6Addr, u16)> for BraidAddr {
    fn from(addr: (std::net::Ipv6Addr, u16)) -> Self {
        Self::Tcp(std::net::SocketAddr::new(
            std::net::IpAddr::V6(addr.0),
            addr.1,
        ))
    }
}

#[cfg(feature = "stream")]
impl From<Utf8PathBuf> for BraidAddr {
    fn from(addr: Utf8PathBuf) -> Self {
        Self::Unix(addr)
    }
}

/// Information about a connection to a stream.
#[cfg(feature = "stream")]
#[derive(Debug, Clone)]
pub struct ConnectionInfo<Addr = BraidAddr> {
    /// The protocol used for this connection.
    pub protocol: Option<Protocol>,

    /// The authority name for the server.
    pub authority: Option<Authority>,

    /// The local address for this connection.
    pub local_addr: Addr,

    /// The remote address for this connection.
    pub remote_addr: Addr,

    /// Buffer size
    pub buffer_size: Option<usize>,

    #[cfg(feature = "tls")]
    /// Transport Layer Security information for this connection.
    pub tls: Option<TlsConnectionInfo>,
}

/// Information about a connection to a stream.
#[cfg(not(feature = "stream"))]
#[derive(Debug, Clone)]
pub struct ConnectionInfo<Addr> {
    /// The protocol used for this connection.
    pub protocol: Option<Protocol>,

    /// The authority name for the server.
    pub authority: Option<Authority>,

    /// The local address for this connection.
    pub local_addr: Addr,

    /// The remote address for this connection.
    pub remote_addr: Addr,

    /// Buffer size
    pub buffer_size: Option<usize>,

    #[cfg(feature = "tls")]
    /// Transport Layer Security information for this connection.
    pub tls: Option<TlsConnectionInfo>,
}

impl<Addr> Default for ConnectionInfo<Addr>
where
    Addr: Default,
{
    fn default() -> Self {
        Self {
            protocol: None,
            authority: None,
            local_addr: Addr::default(),
            remote_addr: Addr::default(),
            buffer_size: None,

            #[cfg(feature = "tls")]
            tls: None,
        }
    }
}

#[cfg(feature = "stream")]
impl ConnectionInfo<BraidAddr> {
    pub(crate) fn duplex(name: Authority, protocol: Option<Protocol>, buffer_size: usize) -> Self {
        ConnectionInfo {
            protocol,
            authority: Some(name),
            local_addr: BraidAddr::Duplex,
            remote_addr: BraidAddr::Duplex,
            buffer_size: Some(buffer_size),

            #[cfg(feature = "tls")]
            tls: None,
        }
    }
}

#[cfg(not(feature = "stream"))]
impl ConnectionInfo<DuplexAddr> {
    pub(crate) fn duplex(name: Authority, protocol: Option<Protocol>, buffer_size: usize) -> Self {
        ConnectionInfo {
            protocol,
            authority: Some(name),
            local_addr: DuplexAddr,
            remote_addr: DuplexAddr,
            buffer_size: Some(buffer_size),

            #[cfg(feature = "tls")]
            tls: None,
        }
    }
}

impl<Addr> ConnectionInfo<Addr> {
    #[cfg(feature = "tls")]
    /// Add tls info to the connection info
    pub(crate) fn tls(self, tls: TlsConnectionInfo) -> Self {
        ConnectionInfo {
            tls: Some(tls),
            ..self
        }
    }

    /// The local address for this connection
    pub fn local_addr(&self) -> &Addr {
        &self.local_addr
    }

    /// The remote address for this connection
    pub fn remote_addr(&self) -> &Addr {
        &self.remote_addr
    }

    /// Map the addresses in this connection info to a new type.
    pub fn map<T, F>(self, f: F) -> ConnectionInfo<T>
    where
        F: Fn(Addr) -> T,
    {
        ConnectionInfo {
            protocol: self.protocol,
            authority: self.authority,
            local_addr: f(self.local_addr),
            remote_addr: f(self.remote_addr),
            buffer_size: self.buffer_size,

            #[cfg(feature = "tls")]
            tls: self.tls,
        }
    }
}

impl<Addr> TryFrom<&TcpStream> for ConnectionInfo<Addr>
where
    Addr: From<std::net::SocketAddr>,
{
    type Error = io::Error;

    fn try_from(stream: &TcpStream) -> Result<Self, Self::Error> {
        let local_addr = stream.local_addr()?;
        let remote_addr = stream.peer_addr()?;

        Ok(Self {
            protocol: None,
            authority: None,
            local_addr: local_addr.into(),
            remote_addr: remote_addr.into(),
            buffer_size: None,

            #[cfg(feature = "tls")]
            tls: None,
        })
    }
}

impl<Addr> TryFrom<&UnixStream> for ConnectionInfo<Addr>
where
    Addr: TryFrom<tokio::net::unix::SocketAddr>,
    Addr::Error: std::error::Error,
{
    type Error = io::Error;

    fn try_from(stream: &UnixStream) -> Result<Self, Self::Error> {
        let local_addr = stream.local_addr()?;
        let remote_addr = stream.peer_addr()?;

        Ok(Self {
            protocol: None,
            authority: None,
            local_addr: local_addr.try_into().expect("unix socket address"),
            remote_addr: remote_addr.try_into().expect("unix socket address"),
            buffer_size: None,

            #[cfg(feature = "tls")]
            tls: None,
        })
    }
}

/// Trait for types which can provide connection information.
pub trait HasConnectionInfo {
    /// The address type for this connection.
    type Addr: fmt::Display + fmt::Debug;

    /// Get the connection information for this stream.
    fn info(&self) -> ConnectionInfo<Self::Addr>;
}

impl HasConnectionInfo for TcpStream {
    type Addr = std::net::SocketAddr;

    fn info(&self) -> ConnectionInfo<Self::Addr> {
        self.try_into()
            .expect("connection info should be available")
    }
}

impl HasConnectionInfo for UnixStream {
    type Addr = Utf8PathBuf;

    fn info(&self) -> ConnectionInfo<Self::Addr> {
        ConnectionInfo {
            local_addr: Utf8PathBuf::from_path_buf(
                self.local_addr().unwrap().as_pathname().unwrap().to_owned(),
            )
            .expect("unix socket address"),
            remote_addr: Utf8PathBuf::from_path_buf(
                self.peer_addr().unwrap().as_pathname().unwrap().to_owned(),
            )
            .expect("unix socket address"),
            ..Default::default()
        }
    }
}
