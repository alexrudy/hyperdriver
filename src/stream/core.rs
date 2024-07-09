//! Core stream type for braid providing [AsyncRead] and [AsyncWrite].

use std::pin::pin;

use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::info::BraidAddr;
use crate::info::{ConnectionInfo, HasConnectionInfo};
use crate::stream::duplex::DuplexStream;
use crate::stream::tcp::TcpStream;
use crate::stream::unix::UnixStream;

/// Dispatching wrapper for potential stream connection types
///
/// Effectively implements enum-dispatch for AsyncRead and AsyncWrite
/// around the stream types which we might use in braid.
///
/// This core type is used in the server and client modules, and so is
/// generic over the TLS stream type (which is different for client and server).
#[derive(Debug)]
#[pin_project(project = BraidCoreProjection)]
pub enum Braid {
    /// A TCP stream
    Tcp(#[pin] TcpStream),

    /// A duplex stream
    Duplex(#[pin] DuplexStream),

    /// A Unix stream
    Unix(#[pin] UnixStream),
}

impl HasConnectionInfo for Braid {
    type Addr = BraidAddr;
    fn info(&self) -> ConnectionInfo<BraidAddr> {
        match self {
            Braid::Tcp(stream) => stream.info().map(BraidAddr::Tcp),
            Braid::Duplex(stream) => {
                <DuplexStream as HasConnectionInfo>::info(stream).map(|_| BraidAddr::Duplex)
            }
            Braid::Unix(stream) => stream.info().map(BraidAddr::Unix),
        }
    }
}

macro_rules! dispatch_core {
    ($driver:ident.$method:ident($($args:expr),+)) => {

        match $driver.project() {
            BraidCoreProjection::Tcp(stream) => stream.$method($($args),+),
            BraidCoreProjection::Duplex(stream) => stream.$method($($args),+),
            BraidCoreProjection::Unix(stream) => stream.$method($($args),+),
        }
    };
}

impl AsyncRead for Braid {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        dispatch_core!(self.poll_read(cx, buf))
    }
}

impl AsyncWrite for Braid {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        dispatch_core!(self.poll_write(cx, buf))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        dispatch_core!(self.poll_flush(cx))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        dispatch_core!(self.poll_shutdown(cx))
    }
}

impl From<TcpStream> for Braid {
    fn from(stream: TcpStream) -> Self {
        Self::Tcp(stream)
    }
}

impl From<DuplexStream> for Braid {
    fn from(stream: DuplexStream) -> Self {
        Self::Duplex(stream)
    }
}

impl From<UnixStream> for Braid {
    fn from(stream: UnixStream) -> Self {
        Self::Unix(stream)
    }
}
