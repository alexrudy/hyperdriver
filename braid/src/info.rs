//! Connection info for braid streams.

use std::fmt;
use std::io;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use http::uri::Authority;
use tokio::net::{TcpStream, UnixStream};

use crate::duplex::DuplexStream;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Protocol {
    Http(http::Version),
    Grpc,
    Websocket,
}

impl Protocol {
    pub fn is_http2(&self) -> bool {
        matches!(self, Self::Http(http::Version::HTTP_2) | Self::Grpc)
    }
}

impl From<http::Version> for Protocol {
    fn from(version: http::Version) -> Self {
        Self::Http(version)
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
}

impl std::fmt::Display for SocketAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp(addr) => write!(f, "{}", addr),
            Self::Duplex => write!(f, "<duplex>"),
            Self::Unix(path) => write!(f, "{}", path),
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
        Ok(Self::Unix(
            addr.as_pathname()
                .map(|path| path.to_owned())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "missing unix socket address")
                })
                .and_then(|path| {
                    Utf8PathBuf::from_path_buf(path).map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "invalid (non-utf8) unix socket address",
                        )
                    })
                })?,
        ))
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
pub struct TLSConnectionInfo {
    pub sni: Option<String>,
    pub validated_sni: bool,
}

impl TLSConnectionInfo {
    pub fn new(sni: Option<String>) -> Self {
        Self {
            sni,
            validated_sni: false,
        }
    }

    pub fn validated_sni(&mut self) -> &mut Self {
        self.validated_sni = true;
        self
    }
}

#[derive(Debug, Clone)]
pub struct TcpConnectionInfo {
    pub local_addr: SocketAddr,
    pub remote_addr: SocketAddr,
}

#[derive(Debug, Clone)]
pub struct UnixConnectionInfo {
    pub local_addr: Option<SocketAddr>,
    pub remote_addr: Option<SocketAddr>,
}

#[derive(Debug, Clone)]
pub struct DuplexConnectionInfo {
    pub authority: Authority,
    pub protocol: Protocol,
}

impl DuplexConnectionInfo {
    pub fn new(authority: Authority, protocol: Protocol) -> Self {
        Self {
            authority,
            protocol,
        }
    }
}

impl From<&DuplexStream> for DuplexConnectionInfo {
    fn from(stream: &DuplexStream) -> Self {
        stream.info().clone()
    }
}

impl TryFrom<&TcpStream> for TcpConnectionInfo {
    type Error = io::Error;
    fn try_from(value: &TcpStream) -> Result<Self, Self::Error> {
        Ok(Self {
            local_addr: value.local_addr()?.into(),
            remote_addr: value.peer_addr()?.into(),
        })
    }
}

impl TryFrom<&UnixStream> for UnixConnectionInfo {
    type Error = io::Error;
    fn try_from(stream: &UnixStream) -> Result<Self, Self::Error> {
        Ok(UnixConnectionInfo {
            local_addr: stream
                .local_addr()?
                .as_pathname()
                .and_then(|path| Utf8PathBuf::from_path_buf(path.to_owned()).ok())
                .map(|path| path.into()),
            remote_addr: stream
                .peer_addr()
                .ok()
                .and_then(|address| address.as_pathname().map(|path| path.to_owned()))
                .and_then(|path| Utf8PathBuf::from_path_buf(path).ok())
                .map(|path| path.into()),
        })
    }
}

#[derive(Debug, Clone)]
enum ConnectionInfoInner {
    NoTls(ConnectionInfoCore),
    Tls(ConnectionInfoCore, TLSConnectionInfo),
}

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    inner: ConnectionInfoInner,
}

impl ConnectionInfo {
    pub(crate) fn with_tls(self, tls: TLSConnectionInfo) -> Self {
        match self.inner {
            ConnectionInfoInner::NoTls(core) => Self {
                inner: ConnectionInfoInner::Tls(core, tls),
            },
            ConnectionInfoInner::Tls(_, _) => panic!("ConnectionInfo::tls called twice"),
        }
    }

    pub(crate) fn new(core: ConnectionInfoCore) -> Self {
        Self {
            inner: ConnectionInfoInner::NoTls(core),
        }
    }

    pub fn local_addr(&self) -> Option<SocketAddr> {
        match &self.inner {
            ConnectionInfoInner::NoTls(core) => core.local_addr(),
            ConnectionInfoInner::Tls(core, _) => core.local_addr(),
        }
    }

    pub fn remote_addr(&self) -> Option<&SocketAddr> {
        match &self.inner {
            ConnectionInfoInner::NoTls(core) => core.remote_addr(),
            ConnectionInfoInner::Tls(core, _) => core.remote_addr(),
        }
    }

    pub fn tls(&self) -> Option<&TLSConnectionInfo> {
        match &self.inner {
            ConnectionInfoInner::NoTls(_) => None,
            ConnectionInfoInner::Tls(_, tls) => Some(tls),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ConnectionInfoCore {
    Tcp(TcpConnectionInfo),
    Duplex(DuplexConnectionInfo),
    Unix(UnixConnectionInfo),
}

impl ConnectionInfoCore {
    pub(crate) fn local_addr(&self) -> Option<SocketAddr> {
        match self {
            ConnectionInfoCore::Tcp(tcp) => Some(tcp.local_addr.clone()),
            ConnectionInfoCore::Duplex(_) => None,
            ConnectionInfoCore::Unix(unix) => unix.local_addr.clone().map(SocketAddr::from),
        }
    }

    pub(crate) fn remote_addr(&self) -> Option<&SocketAddr> {
        match self {
            ConnectionInfoCore::Tcp(tcp) => Some(&tcp.remote_addr),
            ConnectionInfoCore::Duplex(_) => None,
            ConnectionInfoCore::Unix(unix) => unix.remote_addr.as_ref(),
        }
    }
}

pub trait Connection {
    fn info(&self) -> ConnectionInfo;
}

impl Connection for TcpStream {
    fn info(&self) -> ConnectionInfo {
        ConnectionInfo::new(ConnectionInfoCore::Tcp(
            self.try_into()
                .expect("TCP stream should always have both addresses"),
        ))
    }
}

impl Connection for DuplexStream {
    fn info(&self) -> ConnectionInfo {
        ConnectionInfo::new(ConnectionInfoCore::Duplex(self.into()))
    }
}

impl Connection for UnixStream {
    fn info(&self) -> ConnectionInfo {
        ConnectionInfo::new(ConnectionInfoCore::Unix(
            self.try_into()
                .expect("Unix stream should always have both addresses"),
        ))
    }
}
