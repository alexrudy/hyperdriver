//! Connection info for braid streams.

use std::fmt;
use std::io;
use std::str::FromStr;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use http::uri::Authority;
use thiserror::Error;
use tokio::net::{TcpStream, UnixStream};

use crate::tls::info::TlsConnectionInfo;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Protocol {
    Http(http::Version),
    Grpc,
    WebSocket,
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
    pub fn http(version: http::Version) -> Self {
        Self::Http(version)
    }

    pub fn grpc() -> Self {
        Self::Grpc
    }

    pub fn web_socket() -> Self {
        Self::WebSocket
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SocketAddr {
    Tcp(std::net::SocketAddr),
    Duplex,
    Unix(Utf8PathBuf),
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
    pub fn tcp(&self) -> Option<std::net::SocketAddr> {
        match self {
            Self::Tcp(addr) => Some(*addr),
            _ => None,
        }
    }

    pub fn path(&self) -> Option<&Utf8Path> {
        match self {
            Self::Unix(path) => Some(path.as_path()),
            _ => None,
        }
    }

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

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub protocol: Option<Protocol>,
    pub authority: Option<Authority>,
    pub local_addr: SocketAddr,
    pub remote_addr: SocketAddr,

    /// Transport Layer Security information for this connection.
    pub tls: Option<TlsConnectionInfo>,
}

impl ConnectionInfo {
    pub(crate) fn duplex(name: Authority, protocol: Protocol) -> ConnectionInfo {
        ConnectionInfo {
            protocol: Some(protocol),
            authority: Some(name),
            local_addr: SocketAddr::Duplex,
            remote_addr: SocketAddr::Duplex,
            tls: None,
        }
    }

    pub(crate) fn tls(self, tls: TlsConnectionInfo) -> ConnectionInfo {
        ConnectionInfo {
            tls: Some(tls),
            ..self
        }
    }

    pub fn local_addr(&self) -> &SocketAddr {
        &self.local_addr
    }

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
            tls: None,
        })
    }
}

pub trait Connection {
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
