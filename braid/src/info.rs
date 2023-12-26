//! Connection info for braid streams.

use std::fmt;
use std::io;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use http::uri::Authority;
use hyper::server::conn::AddrStream;
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
    fn build(stream: &tokio_rustls::server::TlsStream<AddrStream>) -> TcpConncectionInfo {
        let (stream, server_info) = stream.get_ref();
        let sni = server_info.server_name().map(|s| s.to_string());

        let mut tcp = TcpConncectionInfo::from(stream);
        tcp.tls = Some(Self {
            sni,
            validated_sni: false,
        });
        tcp
    }

    pub fn validated_sni(&mut self) {
        self.validated_sni = true;
    }
}

#[derive(Debug, Clone)]
pub struct TcpConncectionInfo {
    pub local_addr: SocketAddr,
    pub remote_addr: SocketAddr,
    pub tls: Option<TLSConnectionInfo>,
}

impl TcpConncectionInfo {
    pub fn new(
        local_addr: SocketAddr,
        remote_addr: SocketAddr,
        tls: Option<TLSConnectionInfo>,
    ) -> Self {
        Self {
            local_addr,
            remote_addr,
            tls,
        }
    }
}

impl From<&AddrStream> for TcpConncectionInfo {
    fn from(stream: &AddrStream) -> Self {
        let local_addr = stream.local_addr().into();
        let remote_addr = stream.remote_addr().into();

        Self {
            local_addr,
            remote_addr,
            tls: None,
        }
    }
}

impl TryFrom<&TcpStream> for TcpConncectionInfo {
    type Error = io::Error;
    fn try_from(stream: &TcpStream) -> Result<Self, Self::Error> {
        let local_addr = stream.local_addr()?.into();
        let remote_addr = stream.peer_addr()?.into();

        Ok(Self {
            local_addr,
            remote_addr,
            tls: None,
        })
    }
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

#[derive(Debug, Clone)]
pub enum ConnectionInfo {
    Tcp(TcpConncectionInfo),
    Duplex(DuplexConnectionInfo),
    Unix(UnixConnectionInfo),
}

impl ConnectionInfo {
    pub fn local_addr(&self) -> Option<SocketAddr> {
        match self {
            ConnectionInfo::Tcp(tcp) => Some(tcp.local_addr.clone()),
            ConnectionInfo::Duplex(_) => None,
            ConnectionInfo::Unix(unix) => unix.local_addr.clone().map(SocketAddr::from),
        }
    }

    pub fn remote_addr(&self) -> Option<&SocketAddr> {
        match self {
            ConnectionInfo::Tcp(tcp) => Some(&tcp.remote_addr),
            ConnectionInfo::Duplex(_) => None,
            ConnectionInfo::Unix(unix) => unix.remote_addr.as_ref(),
        }
    }
}

impl From<&tokio_rustls::server::TlsStream<AddrStream>> for ConnectionInfo {
    fn from(stream: &tokio_rustls::server::TlsStream<AddrStream>) -> Self {
        ConnectionInfo::Tcp(TLSConnectionInfo::build(stream))
    }
}

impl From<&AddrStream> for ConnectionInfo {
    fn from(stream: &AddrStream) -> Self {
        ConnectionInfo::Tcp(stream.into())
    }
}

impl TryFrom<&TcpStream> for ConnectionInfo {
    type Error = io::Error;
    fn try_from(value: &TcpStream) -> Result<Self, Self::Error> {
        Ok(ConnectionInfo::Tcp(value.try_into()?))
    }
}

impl From<&DuplexStream> for ConnectionInfo {
    fn from(stream: &DuplexStream) -> Self {
        ConnectionInfo::Duplex(stream.info().clone())
    }
}

impl TryFrom<&UnixStream> for ConnectionInfo {
    type Error = io::Error;
    fn try_from(stream: &UnixStream) -> Result<Self, Self::Error> {
        Ok(ConnectionInfo::Unix(UnixConnectionInfo {
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
        }))
    }
}

impl From<TcpConncectionInfo> for ConnectionInfo {
    fn from(value: TcpConncectionInfo) -> Self {
        Self::Tcp(value)
    }
}

impl From<DuplexConnectionInfo> for ConnectionInfo {
    fn from(value: DuplexConnectionInfo) -> Self {
        Self::Duplex(value)
    }
}

impl From<UnixConnectionInfo> for ConnectionInfo {
    fn from(value: UnixConnectionInfo) -> Self {
        Self::Unix(value)
    }
}

pub trait Connection {
    fn remote_addr(&self) -> Option<&SocketAddr>;
    fn local_addr(&self) -> Option<&SocketAddr>;
}
