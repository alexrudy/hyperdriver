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

use camino::Utf8Path;
use camino::Utf8PathBuf;
use tokio::io::{AsyncRead, AsyncWrite};
#[cfg(all(feature = "server", feature = "stream"))]
use tokio::net::UnixListener;

use crate::info::HasConnectionInfo;
#[cfg(all(feature = "server", feature = "stream"))]
use crate::server::Accept;

/// Connection address for a unix domain socket.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct UnixAddr {
    path: Option<Utf8PathBuf>,
}

impl UnixAddr {
    /// Does this socket have a name
    pub fn is_named(&self) -> bool {
        self.path.is_some()
    }

    /// Get the path of this socket.
    pub fn path(&self) -> Option<&Utf8Path> {
        self.path.as_deref()
    }

    /// Create a new address from a path.
    pub fn from_pathbuf(path: Utf8PathBuf) -> Self {
        Self { path: Some(path) }
    }

    /// Create a new address without a path.
    pub fn unnamed() -> Self {
        Self { path: None }
    }
}

impl fmt::Display for UnixAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(path) = self.path() {
            write!(f, "unix://{}", path)
        } else {
            write!(f, "unix://")
        }
    }
}

impl TryFrom<std::os::unix::net::SocketAddr> for UnixAddr {
    type Error = io::Error;
    fn try_from(addr: std::os::unix::net::SocketAddr) -> Result<Self, Self::Error> {
        Ok(Self {
            path: addr
                .as_pathname()
                .map(|p| {
                    Utf8Path::from_path(p).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "not a utf-8 path")
                    })
                })
                .transpose()?
                .map(|path| path.to_owned()),
        })
    }
}

impl TryFrom<tokio::net::unix::SocketAddr> for UnixAddr {
    type Error = io::Error;
    fn try_from(addr: tokio::net::unix::SocketAddr) -> Result<Self, Self::Error> {
        Ok(Self {
            path: addr
                .as_pathname()
                .map(|p| {
                    Utf8Path::from_path(p).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "not a utf-8 path")
                    })
                })
                .transpose()?
                .map(|path| path.to_owned()),
        })
    }
}

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
        let path = path.as_ref();
        let stream = tokio::net::UnixStream::connect(path).await?;
        Ok(Self::new(
            stream,
            Some(UnixAddr::from_pathbuf(
                Utf8PathBuf::from_path_buf(path.to_path_buf()).map_err(|path| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unix path is not utf-8: {}", path.display()),
                    )
                })?,
            )),
        ))
    }

    /// Create a pair of connected `UnixStream`s. See `tokio::net::UnixStream::pair`.
    pub fn pair() -> io::Result<(Self, Self)> {
        let (a, b) = tokio::net::UnixStream::pair()?;
        Ok((
            Self::new(a, Some(UnixAddr::unnamed())),
            Self::new(b, Some(UnixAddr::unnamed())),
        ))
    }

    /// Create a new `UnixStream` from an existing `tokio::net::UnixStream` for a
    /// connection. Most of the time, the remote addr should also be passed here,
    /// but there may be cases when you are handed the stream without the remote
    /// addr.
    pub fn new(inner: tokio::net::UnixStream, remote: Option<UnixAddr>) -> Self {
        Self {
            stream: inner,
            remote,
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
            res.and_then(|(stream, remote)| Ok(UnixStream::new(stream, Some(remote.try_into()?))))
        })
    }
}
