//! Core stream type for braid providing [AsyncRead] and [AsyncWrite].

use std::pin::pin;
use std::task::Poll;

use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UnixStream};

use crate::stream::duplex::DuplexStream;
use crate::stream::info::{Connection, ConnectionInfo};

use super::tls::TlsHandshakeStream;

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
    Tcp(#[pin] TcpStream),
    Duplex(#[pin] DuplexStream),
    Unix(#[pin] UnixStream),
}

impl Connection for Braid {
    fn info(&self) -> ConnectionInfo {
        match self {
            Braid::Tcp(stream) => stream.info(),
            Braid::Duplex(stream) => <DuplexStream as Connection>::info(stream),
            Braid::Unix(stream) => stream.info(),
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

#[derive(Debug)]
#[pin_project(project=BraidProjection)]
pub enum TlsBraid<Tls, NoTls> {
    NoTls(#[pin] NoTls),
    Tls(#[pin] Tls),
}

impl<Tls, NoTls> TlsHandshakeStream for TlsBraid<Tls, NoTls>
where
    Tls: TlsHandshakeStream + Unpin,
    NoTls: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_handshake(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        match self {
            TlsBraid::NoTls(_) => Poll::Ready(Ok(())),
            TlsBraid::Tls(ref mut stream) => stream.poll_handshake(cx),
        }
    }
}

macro_rules! dispatch {
    ($driver:ident.$method:ident($($args:expr),+)) => {

        match $driver.project() {
            BraidProjection::NoTls(stream) => stream.$method($($args),+),
            BraidProjection::Tls(stream) => stream.$method($($args),+),
        }
    };
}

impl<Tls, NoTls> AsyncRead for TlsBraid<Tls, NoTls>
where
    Tls: AsyncRead,
    NoTls: AsyncRead,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        dispatch!(self.poll_read(cx, buf))
    }
}

impl<Tls, NoTls> AsyncWrite for TlsBraid<Tls, NoTls>
where
    Tls: AsyncWrite,
    NoTls: AsyncWrite,
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

impl<Tls, NoTls> From<NoTls> for TlsBraid<Tls, NoTls> {
    fn from(stream: NoTls) -> Self {
        Self::NoTls(stream)
    }
}
