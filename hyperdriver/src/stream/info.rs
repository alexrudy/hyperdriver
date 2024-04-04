//! Connection info for braid streams.

use std::fmt;
use std::io;
use std::str::FromStr;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use http::uri::Authority;
use thiserror::Error;
use tokio::net::{TcpStream, UnixStream};

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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SocketAddr {
    /// A TCP socket address.
    Tcp(std::net::SocketAddr),

    /// Represents a duplex connection which has no address.
    Duplex,

    /// A Unix socket address.
    Unix(Utf8PathBuf),

    /// Represents a Unix socket connection which has no address.
    UnixUnnamed,
}

impl std::fmt::Display for SocketAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp(addr) => write!(f, "{}", addr),
            Self::Duplex => write!(f, "<duplex>"),
            Self::Unix(path) => write!(f, "{}", path),
            Self::UnixUnnamed => write!(f, "<unnamed>"),
        }
    }
}

impl SocketAddr {
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

impl From<std::net::SocketAddr> for SocketAddr {
    fn from(addr: std::net::SocketAddr) -> Self {
        Self::Tcp(make_canonical(addr))
    }
}

impl TryFrom<tokio::net::unix::SocketAddr> for SocketAddr {
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

impl From<(std::net::IpAddr, u16)> for SocketAddr {
    fn from(addr: (std::net::IpAddr, u16)) -> Self {
        Self::Tcp(std::net::SocketAddr::new(addr.0, addr.1))
    }
}

impl From<(std::net::Ipv4Addr, u16)> for SocketAddr {
    fn from(addr: (std::net::Ipv4Addr, u16)) -> Self {
        Self::Tcp(std::net::SocketAddr::new(
            std::net::IpAddr::V4(addr.0),
            addr.1,
        ))
    }
}

impl From<(std::net::Ipv6Addr, u16)> for SocketAddr {
    fn from(addr: (std::net::Ipv6Addr, u16)) -> Self {
        Self::Tcp(std::net::SocketAddr::new(
            std::net::IpAddr::V6(addr.0),
            addr.1,
        ))
    }
}

impl From<Utf8PathBuf> for SocketAddr {
    fn from(addr: Utf8PathBuf) -> Self {
        Self::Unix(addr)
    }
}

/// Information about a connection to a Braid stream.
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    /// The protocol used for this connection.
    pub protocol: Option<Protocol>,

    /// The authority name for the server.
    pub authority: Option<Authority>,

    /// The local address for this connection.
    pub local_addr: SocketAddr,

    /// The remote address for this connection.
    pub remote_addr: SocketAddr,

    /// Buffer size
    pub buffer_size: Option<usize>,

    /// Transport Layer Security information for this connection.
    pub tls: Option<TlsConnectionInfo>,
}

impl ConnectionInfo {
    pub(crate) fn duplex(
        name: Authority,
        protocol: Option<Protocol>,
        buffer_size: usize,
    ) -> ConnectionInfo {
        ConnectionInfo {
            protocol,
            authority: Some(name),
            local_addr: SocketAddr::Duplex,
            remote_addr: SocketAddr::Duplex,
            buffer_size: Some(buffer_size),
            tls: None,
        }
    }

    pub(crate) fn tls(self, tls: TlsConnectionInfo) -> ConnectionInfo {
        ConnectionInfo {
            tls: Some(tls),
            ..self
        }
    }

    /// The local address for this connection
    pub fn local_addr(&self) -> &SocketAddr {
        &self.local_addr
    }

    /// The remote address for this connection
    pub fn remote_addr(&self) -> &SocketAddr {
        &self.remote_addr
    }
}

impl TryFrom<&TcpStream> for ConnectionInfo {
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
            tls: None,
        })
    }
}

impl TryFrom<&UnixStream> for ConnectionInfo {
    type Error = io::Error;

    fn try_from(stream: &UnixStream) -> Result<Self, Self::Error> {
        let local_addr = stream.local_addr()?;
        let remote_addr = stream.peer_addr()?;

        Ok(Self {
            protocol: None,
            authority: None,
            local_addr: local_addr.try_into()?,
            remote_addr: remote_addr.try_into()?,
            buffer_size: None,
            tls: None,
        })
    }
}

/// Trait for types which can provide connection information.
pub trait Connection {
    /// Get the connection information for this stream.
    fn info(&self) -> ConnectionInfo;
}

impl Connection for TcpStream {
    fn info(&self) -> ConnectionInfo {
        self.try_into()
            .expect("connection info should be available")
    }
}

impl Connection for UnixStream {
    fn info(&self) -> ConnectionInfo {
        self.try_into()
            .expect("connection info should be available")
    }
}
