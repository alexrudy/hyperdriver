//! Connection Information

use std::convert::Infallible;
use std::fmt;
#[cfg(feature = "stream")]
use std::io;
use std::str::FromStr;

#[cfg(feature = "stream")]
use camino::Utf8Path;
#[cfg(feature = "stream")]
use camino::Utf8PathBuf;

#[doc(hidden)]
pub use chateau::stream::duplex::DuplexAddr;

#[doc(hidden)]
pub use chateau::stream::unix::UnixAddr;

/// The transport protocol used for a connection.
///
/// This is for informational purposes only, and can be used
/// to select the appropriate transport when a transport should
/// be pre-negotiated (e.g. ALPN or a Duplex socket).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
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
            Self::Http(version) => write!(f, "{version:?}"),
            Self::Grpc => write!(f, "gRPC"),
            Self::WebSocket => write!(f, "WebSocket"),
            Self::Other(s) => write!(f, "{s}"),
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

impl FromStr for Protocol {
    type Err = Infallible;

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
    Unix(UnixAddr),
}

#[cfg(feature = "stream")]
impl std::fmt::Display for BraidAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp(addr) => write!(f, "{addr}"),
            Self::Duplex => write!(f, "<duplex>"),
            Self::Unix(path) => write!(f, "{path}"),
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
            Self::Unix(addr) => addr.path().map(Utf8Path::from_path).flatten(),
            _ => None,
        }
    }

    /// Returns the canonical TCP address, if this is a TCP socket address.
    pub fn canonical(self) -> Self {
        match self {
            Self::Tcp(addr) => Self::Tcp(addr),
            _ => self,
        }
    }
}

#[cfg(feature = "stream")]
impl From<std::net::SocketAddr> for BraidAddr {
    fn from(addr: std::net::SocketAddr) -> Self {
        Self::Tcp(addr)
    }
}

#[cfg(feature = "stream")]
impl TryFrom<tokio::net::unix::SocketAddr> for BraidAddr {
    type Error = io::Error;
    fn try_from(addr: tokio::net::unix::SocketAddr) -> Result<Self, Self::Error> {
        Ok(Self::Unix(addr.try_into()?))
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
        Self::Unix(UnixAddr::from_pathbuf(addr.into_std_path_buf()))
    }
}

#[cfg(feature = "stream")]
impl From<UnixAddr> for BraidAddr {
    fn from(addr: UnixAddr) -> Self {
        Self::Unix(addr)
    }
}

#[cfg(feature = "stream")]
impl From<DuplexAddr> for BraidAddr {
    fn from(_: DuplexAddr) -> Self {
        Self::Duplex
    }
}

#[cfg(test)]
mod tests {

    #[cfg(feature = "client")]
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use http::Version;
    #[cfg(feature = "client")]
    use tokio::net::TcpListener;
    use tokio::net::UnixListener;

    #[cfg(feature = "client")]
    use chateau::stream::tcp::TcpStream;
    use chateau::{
        info::{ConnectionInfo, HasConnectionInfo},
        stream::unix::UnixStream,
    };

    use super::*;

    #[test]
    fn protocol_display() {
        assert_eq!(Protocol::http(Version::HTTP_11).to_string(), "HTTP/1.1");
        assert_eq!(Protocol::http(Version::HTTP_2).to_string(), "HTTP/2.0");
        assert_eq!(Protocol::http(Version::HTTP_3).to_string(), "HTTP/3.0");
        assert_eq!(Protocol::http(Version::HTTP_10).to_string(), "HTTP/1.0");
        assert_eq!(Protocol::grpc().to_string(), "gRPC");
        assert_eq!(Protocol::web_socket().to_string(), "WebSocket");
    }

    #[test]
    fn parse_protocol() {
        assert_eq!(
            Protocol::from_str("http/1.1").unwrap(),
            Protocol::http(Version::HTTP_11)
        );
        assert_eq!(
            Protocol::from_str("h2").unwrap(),
            Protocol::http(Version::HTTP_2)
        );
        assert_eq!(
            Protocol::from_str("h3").unwrap(),
            Protocol::http(Version::HTTP_3)
        );
        assert_eq!(
            Protocol::from_str("http/1.0").unwrap(),
            Protocol::http(Version::HTTP_10)
        );
        assert_eq!(Protocol::from_str("gRPC").unwrap(), Protocol::grpc());
        assert_eq!(
            Protocol::from_str("WebSocket").unwrap(),
            Protocol::web_socket()
        );
        assert_eq!(
            Protocol::from_str("foo").unwrap(),
            Protocol::Other("foo".into())
        )
    }

    #[test]
    fn connection_info_default() {
        let info = ConnectionInfo::<DuplexAddr>::default();

        assert_eq!(info.local_addr, DuplexAddr::new());
        assert_eq!(info.remote_addr, DuplexAddr::new());
    }

    #[test]
    fn unix_addr() {
        let addr = UnixAddr::from_pathbuf("/tmp/foo.sock".into());
        assert_eq!(addr.path(), Some(std::path::Path::new("/tmp/foo.sock")));

        let addr = UnixAddr::unnamed();
        assert_eq!(addr.path(), None);
    }

    #[test]
    fn connection_info_map() {
        let info = ConnectionInfo {
            local_addr: "local",
            remote_addr: "remote",
        };

        let mapped = info.map(|addr| addr.to_string());

        assert_eq!(mapped.local_addr, "local".to_string());
    }

    #[tokio::test]
    async fn unix_connection_info_unnamed() {
        let (a, _) = UnixStream::pair().expect("pair");

        let info: ConnectionInfo<UnixAddr> = a.info();
        assert_eq!(info.local_addr(), &UnixAddr::unnamed());
    }

    #[tokio::test]
    async fn unix_connection_info_named() {
        let tmp = tempfile::TempDir::with_prefix("unix-connection-info").unwrap();
        tokio::fs::create_dir_all(&tmp).await.unwrap();
        let path = tmp.path().join("socket.sock");

        let listener = UnixListener::bind(&path).unwrap();

        let conn = UnixStream::connect(&path).await.unwrap();

        let info: ConnectionInfo<UnixAddr> = conn.info();

        assert_eq!(
            info.remote_addr(),
            &UnixAddr::from_pathbuf(path.try_into().unwrap())
        );

        drop(listener);
    }

    #[tokio::test]
    #[cfg(feature = "client")]
    async fn tcp_connection_info() {
        let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();

        let conn = TcpStream::connect(addr).await.unwrap();

        let info: ConnectionInfo<std::net::SocketAddr> = conn.info();
        assert_eq!(info.remote_addr().ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(info.remote_addr().port(), addr.port());
    }
}
