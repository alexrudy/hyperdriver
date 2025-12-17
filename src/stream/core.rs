//! Core stream type for braid providing [AsyncRead] and [AsyncWrite].

use chateau::client::pool::PoolableStream;
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::info::BraidAddr;
use chateau::info::{ConnectionInfo, HasConnectionInfo};
use chateau::stream::duplex::DuplexStream;
use chateau::stream::tcp::TcpStream;
use chateau::stream::unix::UnixStream;

#[derive(Debug)]
#[pin_project(project = BraidCoreProjection)]
enum BraidCore {
    /// A TCP stream
    Tcp(#[pin] TcpStream),

    /// A duplex stream
    Duplex(#[pin] DuplexStream),

    /// A Unix stream
    Unix(#[pin] UnixStream),
}

/// Dispatching wrapper for potential stream connection types
///
/// Effectively implements enum-dispatch for AsyncRead and AsyncWrite
/// around the stream types which we might use in braid.
///
/// This core type is used in the server and client modules, and so is
/// generic over the TLS stream type (which is different for client and server).
#[derive(Debug)]
#[pin_project]
pub struct Braid {
    #[pin]
    inner: BraidCore,
}

impl HasConnectionInfo for Braid {
    type Addr = BraidAddr;
    fn info(&self) -> ConnectionInfo<BraidAddr> {
        match &self.inner {
            BraidCore::Tcp(stream) => stream.info().map(BraidAddr::Tcp),
            BraidCore::Duplex(stream) => {
                <DuplexStream as HasConnectionInfo>::info(stream).map(|_| BraidAddr::Duplex)
            }
            BraidCore::Unix(stream) => stream.info().map(BraidAddr::Unix),
        }
    }
}

macro_rules! dispatch_core {
    (pin $driver:ident.$method:ident($($args:expr),*)) => {

        match $driver.project().inner.project() {
            BraidCoreProjection::Tcp(stream) => stream.$method($($args),*),
            BraidCoreProjection::Duplex(stream) => stream.$method($($args),*),
            BraidCoreProjection::Unix(stream) => stream.$method($($args),*),
        }
    };

    ($driver:ident.$method:ident($($args:expr),*)) => {

        match &$driver.inner {
            BraidCore::Tcp(stream) => stream.$method($($args),*),
            BraidCore::Duplex(stream) => stream.$method($($args),*),
            BraidCore::Unix(stream) => stream.$method($($args),*),
        }
    };
}

impl PoolableStream for Braid {
    fn can_share(&self) -> bool {
        dispatch_core!(self.can_share())
    }
}

impl AsyncRead for Braid {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        dispatch_core!(pin self.poll_read(cx, buf))
    }
}

impl AsyncWrite for Braid {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        dispatch_core!(pin self.poll_write(cx, buf))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        dispatch_core!(pin self.poll_flush(cx))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        dispatch_core!(pin self.poll_shutdown(cx))
    }
}

impl From<TcpStream> for Braid {
    fn from(stream: TcpStream) -> Self {
        Self {
            inner: BraidCore::Tcp(stream),
        }
    }
}

impl From<DuplexStream> for Braid {
    fn from(stream: DuplexStream) -> Self {
        Self {
            inner: BraidCore::Duplex(stream),
        }
    }
}

impl From<UnixStream> for Braid {
    fn from(stream: UnixStream) -> Self {
        Self {
            inner: BraidCore::Unix(stream),
        }
    }
}
