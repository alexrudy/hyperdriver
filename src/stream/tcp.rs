//! TCP Stream implementation with better address semantics for servers.
//!
//! This module provides a `TcpStream` type that wraps `tokio::net::TcpStream` with
//! better address semantics for servers. When a server accepts a connection, it
//! returns the associated `SocketAddr` along side the stream. On some platforms,
//! this information is not available after the connection is established via
//! `TcpStream::peer_addr`. This module provides a way to retain this information
//! for the lifetime of the stream.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::ops::Deref;
use std::ops::DerefMut;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite};
#[cfg(feature = "server")]
pub use tokio::net::TcpListener;
use tokio::net::ToSocketAddrs;

use crate::info::HasConnectionInfo;
#[cfg(feature = "server")]
use crate::server::Accept;

/// Canonicalize a socket address, converting IPv4 addresses which are
/// mapped into IPv6 addresses into standard IPv4 addresses.
pub(crate) fn make_canonical(addr: std::net::SocketAddr) -> std::net::SocketAddr {
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

/// A TCP Stream, wrapping `tokio::net::TcpStream` with better
/// address semantics for servers.
#[pin_project::pin_project]
pub struct TcpStream {
    #[pin]
    stream: tokio::net::TcpStream,
    remote: Option<SocketAddr>,
}

impl fmt::Debug for TcpStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.stream.fmt(f)
    }
}

impl TcpStream {
    /// Connect to a remote address. See `tokio::net::TcpStream::connect`.
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let stream = tokio::net::TcpStream::connect(addr).await?;
        Ok(Self::client(stream))
    }

    /// Create a new `TcpStream` from an existing `tokio::net::TcpStream` for a client
    /// connection. Client connections should have valid `peer_addr` and `local_addr`.
    pub fn client(inner: tokio::net::TcpStream) -> Self {
        Self {
            stream: inner,
            remote: None,
        }
    }

    /// Create a new `TcpStream` from an existing `tokio::net::TcpStream` for a server
    /// connection. Server connections should have a valid `local_addr` but may not have a
    /// `peer_addr`, hence the remote address must be provided.
    pub fn server(inner: tokio::net::TcpStream, remote: SocketAddr) -> Self {
        Self {
            stream: inner,
            remote: Some(make_canonical(remote)),
        }
    }

    /// Remote address of the connection. See `tokio::net::TcpStream::peer_addr`.
    ///
    /// For servers, this will return the remote address provided when creating the stream,
    /// instead of an `io::Error`.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        match self.remote {
            Some(addr) => Ok(addr),
            None => self.stream.peer_addr().map(make_canonical),
        }
    }

    /// Local address of the connection. See `tokio::net::TcpStream::local_addr`.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.stream.local_addr().map(make_canonical)
    }

    /// Unwraps the `TcpStream`, returning the inner `tokio::net::TcpStream`.
    pub fn into_inner(self) -> tokio::net::TcpStream {
        self.stream
    }
}

impl Deref for TcpStream {
    type Target = tokio::net::TcpStream;
    fn deref(&self) -> &Self::Target {
        &self.stream
    }
}

impl DerefMut for TcpStream {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.stream
    }
}

impl HasConnectionInfo for TcpStream {
    type Addr = SocketAddr;
    fn info(&self) -> crate::info::ConnectionInfo<Self::Addr> {
        let remote_addr = match self.remote {
            Some(addr) => addr,
            None => self
                .stream
                .peer_addr()
                .expect("peer_addr is available for stream"),
        };

        crate::info::ConnectionInfo {
            local_addr: self
                .stream
                .local_addr()
                .expect("local_addr is available for stream"),
            remote_addr,
        }
    }
}

#[cfg(feature = "client")]
impl crate::client::pool::PoolableStream for TcpStream {
    fn can_share(&self) -> bool {
        false
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.project().stream.poll_read(cx, buf)
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        self.project().stream.poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.project().stream.poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.project().stream.poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        self.project().stream.poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }
}

#[cfg(feature = "server")]
impl Accept for TcpListener {
    type Conn = TcpStream;
    type Error = io::Error;

    fn poll_accept(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<Self::Conn>> {
        TcpListener::poll_accept(self.get_mut(), cx)
            .map(|res| res.map(|(stream, remote)| TcpStream::server(stream, remote)))
    }
}

#[cfg(test)]
mod tests {
    use crate::info::HasConnectionInfo as _;

    use super::TcpStream;

    #[tokio::test]
    async fn test_tcp_streams() {
        let listener = tokio::net::TcpListener::bind("0:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, remote) = listener.accept().await.unwrap();

            let stream = TcpStream::server(stream, remote);
            let addr = stream.peer_addr().unwrap();
            assert_eq!(addr, remote);
            let addr = stream.info().remote_addr;
            assert_eq!(addr, remote);
        });

        let client = tokio::spawn(async move {
            let stream = TcpStream::connect(addr).await.unwrap();
            let peer = stream.peer_addr().unwrap();
            assert_eq!(addr.port(), peer.port());
        });

        let (s, c) = tokio::join!(server, client);
        s.unwrap();
        c.unwrap();
    }
}
