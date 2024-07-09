//! Unix Stream implementation with better address semantics for servers.
//!
//! This module provides a `UnixStream` type that wraps `tokio::net::UnixStream` with
//! better address semantics for servers. When a server accepts a connection, it
//! returns the associated `SocketAddr` along side the stream. On some platforms,
//! this information is not available after the connection is established via
//! `UnixStream::peer_addr`. This module provides a way to retain this information
//! for the lifetime of the stream.

use std::fmt;
use std::io;
use std::ops::Deref;
use std::ops::DerefMut;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite};
#[cfg(all(feature = "server", feature = "stream"))]
use tokio::net::UnixListener;

use crate::info::HasConnectionInfo;
use crate::info::UnixAddr;
#[cfg(all(feature = "server", feature = "stream"))]
use crate::server::Accept;

/// A Unix Stream, wrapping `tokio::net::UnixStream` with better
/// address semantics for servers.
#[pin_project::pin_project]
pub struct UnixStream {
    #[pin]
    stream: tokio::net::UnixStream,
    remote: Option<UnixAddr>,
}

impl fmt::Debug for UnixStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.stream.fmt(f)
    }
}

impl UnixStream {
    /// Connect to a remote address. See `tokio::net::UnixStream::connect`.
    pub async fn connect<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let stream = tokio::net::UnixStream::connect(path).await?;
        Ok(Self::client(stream))
    }

    /// Create a pair of connected `UnixStream`s. See `tokio::net::UnixStream::pair`.
    pub fn pair() -> io::Result<(Self, Self)> {
        let (a, b) = tokio::net::UnixStream::pair()?;
        Ok((
            Self::server(a, UnixAddr::unnamed()),
            Self::server(b, UnixAddr::unnamed()),
        ))
    }

    /// Create a new `UnixStream` from an existing `tokio::net::UnixStream` for a client
    /// connection. Client connections should have valid `peer_addr` and `local_addr`.
    pub fn client(inner: tokio::net::UnixStream) -> Self {
        Self {
            stream: inner,
            remote: None,
        }
    }

    /// Create a new `UnixStream` from an existing `tokio::net::UnixStream` for a server
    /// connection. Server connections should have a valid `local_addr` but may not have a
    /// `peer_addr`, hence the remote address must be provided.
    pub fn server(inner: tokio::net::UnixStream, remote: UnixAddr) -> Self {
        Self {
            stream: inner,
            remote: Some(remote),
        }
    }

    /// Local address of the connection. See `tokio::net::UnixStream::local_addr`.
    pub fn local_addr(&self) -> io::Result<UnixAddr> {
        self.stream.local_addr().and_then(UnixAddr::try_from)
    }

    /// Remote address of the connection. See `tokio::net::UnixStream::peer_addr`.
    ///
    /// For servers, this will return the remote address provided when creating the stream,
    /// instead of an `io::Error`.
    pub fn peer_addr(&self) -> io::Result<UnixAddr> {
        match &self.remote {
            Some(addr) => Ok(addr.clone()),
            None => self.stream.peer_addr().and_then(UnixAddr::try_from),
        }
    }

    /// Unwraps the `UnixStream`, returning the inner `tokio::net::UnixStream`.
    pub fn into_inner(self) -> tokio::net::UnixStream {
        self.stream
    }
}

impl Deref for UnixStream {
    type Target = tokio::net::UnixStream;
    fn deref(&self) -> &Self::Target {
        &self.stream
    }
}

impl DerefMut for UnixStream {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.stream
    }
}

impl HasConnectionInfo for UnixStream {
    type Addr = UnixAddr;
    fn info(&self) -> crate::info::ConnectionInfo<Self::Addr> {
        let remote_addr = self
            .peer_addr()
            .expect("peer_addr is available for unix stream");
        let local_addr = self
            .local_addr()
            .expect("local_addr is available for unix stream");

        crate::info::ConnectionInfo {
            local_addr,
            remote_addr,
            buffer_size: None,
        }
    }
}

impl AsyncRead for UnixStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.project().stream.poll_read(cx, buf)
    }
}

impl AsyncWrite for UnixStream {
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

#[cfg(all(feature = "server", feature = "stream"))]
impl Accept for UnixListener {
    type Conn = UnixStream;
    type Error = io::Error;

    fn poll_accept(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<Self::Conn>> {
        UnixListener::poll_accept(self.get_mut(), cx).map(|res| {
            res.and_then(|(stream, remote)| Ok(UnixStream::server(stream, remote.try_into()?)))
        })
    }
}
