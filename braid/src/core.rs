//! Core stream type for braid providing [AsyncRead] and [AsyncWrite].

use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UnixStream};

use crate::duplex::DuplexStream;

/// Dispatching wrapper for potential stream connection types
///
/// Effectively implements enum-dispatch for AsyncRead and AsyncWrite
/// around the stream types which we might use in braid.
///
/// This core type is used in the server and client modules, and so is
/// generic over the TLS stream type (which is different for client and server).
#[derive(Debug)]
#[pin_project(project = BraidProjection)]
pub(crate) enum Braid<Tls> {
    Tcp(#[pin] TcpStream),
    Duplex(#[pin] DuplexStream),
    Tls(#[pin] Tls),
    Unix(#[pin] UnixStream),
}

macro_rules! dispatch {
    ($driver:ident.$method:ident($($args:expr),+)) => {
        match $driver.project() {
            BraidProjection::Tcp(stream) => stream.$method($($args),+),
            BraidProjection::Duplex(stream) => stream.$method($($args),+),
            BraidProjection::Tls(stream) => stream.$method($($args),+),
            BraidProjection::Unix(stream) => stream.$method($($args),+),
        }
    };
}

impl<Tls> AsyncRead for Braid<Tls>
where
    Tls: AsyncRead,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        dispatch!(self.poll_read(cx, buf))
    }
}

impl<Tls> AsyncWrite for Braid<Tls>
where
    Tls: AsyncWrite,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        dispatch!(self.poll_write(cx, buf))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        dispatch!(self.poll_flush(cx))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        dispatch!(self.poll_shutdown(cx))
    }
}
