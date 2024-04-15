//! Server side of the Braid stream
//!
//! The server and client are differentiated for TLS support, but otherwise,
//! TCP and Duplex streams are the same whether they are server or client.

use std::io;
use std::task::{Context, Poll};

use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};
#[cfg(feature = "stream")]
use tokio::net::{TcpStream, UnixStream};

#[cfg(feature = "stream")]
use crate::stream::core::Braid;
#[cfg(feature = "stream")]
use crate::stream::duplex::DuplexStream;
use crate::stream::info::{ConnectionInfo, HasConnectionInfo};

#[cfg(feature = "tls")]
use crate::stream::tls::info::TlsConnectionInfoReciever;

#[cfg(feature = "tls")]
use crate::stream::core::TlsBraid;

#[cfg(feature = "tls")]
use crate::stream::tls::server::TlsStream;

mod acceptor;
mod connector;

pub use acceptor::Acceptor;
pub use connector::{Connection, StartConnectionInfoLayer, StartConnectionInfoService};

#[cfg(feature = "stream")]
use super::info::BraidAddr;

#[cfg(feature = "tls")]
use super::tls::TlsHandshakeStream;

#[derive(Debug, Clone)]
enum ConnectionInfoState<Addr> {
    #[cfg(feature = "tls")]
    Handshake(TlsConnectionInfoReciever<Addr>),
    Connected(ConnectionInfo<Addr>),
}

impl<Addr> ConnectionInfoState<Addr>
where
    Addr: Clone,
{
    async fn recv(&self) -> io::Result<ConnectionInfo<Addr>> {
        match self {
            #[cfg(feature = "tls")]
            ConnectionInfoState::Handshake(rx) => rx.recv().await,
            ConnectionInfoState::Connected(info) => Ok(info.clone()),
        }
    }
}

/// An async generator of new connections
pub trait Accept {
    /// The connection type for this acceptor
    type Conn: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static;

    /// The error type for this acceptor
    type Error: Into<Box<dyn std::error::Error + Send + Sync>>;

    /// Poll for a new connection
    fn poll_accept(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Conn, Self::Error>>;
}

/// Dispatching wrapper for potential stream connection types for servers
#[cfg(feature = "stream")]
#[derive(Debug)]
#[pin_project]
pub struct Stream<IO = Braid>
where
    IO: HasConnectionInfo,
{
    info: ConnectionInfoState<IO::Addr>,

    #[cfg(feature = "tls")]
    #[pin]
    inner: TlsBraid<TlsStream<IO>, IO>,

    #[cfg(not(feature = "tls"))]
    #[pin]
    inner: IO,
}

#[cfg(not(feature = "stream"))]
#[derive(Debug)]
#[pin_project]
pub struct Stream<IO>
where
    IO: HasConnectionInfo,
{
    info: ConnectionInfoState<IO::Addr>,

    #[cfg(feature = "tls")]
    #[pin]
    inner: TlsBraid<TlsStream<IO>, IO>,

    #[cfg(not(feature = "tls"))]
    #[pin]
    inner: IO,
}

impl<IO> Stream<IO>
where
    IO: HasConnectionInfo,
{
    /// Create a new stream from an inner stream, without TLS
    pub fn new(inner: IO) -> Self {
        Stream {
            info: ConnectionInfoState::Connected(inner.info()),

            #[cfg(feature = "tls")]
            inner: TlsBraid::NoTls(inner),

            #[cfg(not(feature = "tls"))]
            inner,
        }
    }
}

impl<IO> Stream<IO>
where
    IO: HasConnectionInfo,
    IO::Addr: Clone,
{
    /// Get the connection info for this stream
    ///
    /// This will block until the handshake completes for
    /// TLS connections.
    pub async fn info(&self) -> io::Result<ConnectionInfo<IO::Addr>> {
        match &self.info {
            #[cfg(feature = "tls")]
            ConnectionInfoState::Handshake(rx) => rx.recv().await,
            ConnectionInfoState::Connected(info) => Ok(info.clone()),
        }
    }

    /// Get the remote address for this stream.
    ///
    /// This can be done before the TLS handshake completes.
    pub fn remote_addr(&self) -> &IO::Addr {
        match &self.info {
            #[cfg(feature = "tls")]
            ConnectionInfoState::Handshake(rx) => rx.remote_addr(),
            ConnectionInfoState::Connected(info) => info.remote_addr(),
        }
    }
}

#[cfg(feature = "tls")]
impl<IO> TlsHandshakeStream for Stream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    IO::Addr: Unpin,
{
    fn poll_handshake(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match &mut self.inner {
            TlsBraid::Tls(stream) => stream.poll_handshake(cx),
            TlsBraid::NoTls(_) => Poll::Ready(Ok(())),
        }
    }
}

impl<IO> HasConnectionInfo for Stream<IO>
where
    IO: HasConnectionInfo,
    IO::Addr: Clone,
{
    type Addr = IO::Addr;
    fn info(&self) -> ConnectionInfo<IO::Addr> {
        match &self.info {
            #[cfg(feature = "tls")]
            ConnectionInfoState::Handshake(_) => {
                panic!("connection info is not avaialble before the handshake completes")
            }
            ConnectionInfoState::Connected(info) => info.clone(),
        }
    }
}

#[cfg(feature = "tls")]
impl<IO> From<TlsStream<IO>> for Stream<IO>
where
    IO: HasConnectionInfo,
    IO::Addr: Clone,
{
    fn from(stream: TlsStream<IO>) -> Self {
        Stream {
            info: ConnectionInfoState::Handshake(stream.rx.clone()),
            inner: crate::stream::core::TlsBraid::Tls(stream),
        }
    }
}

#[cfg(feature = "stream")]
impl From<TcpStream> for Stream {
    fn from(stream: TcpStream) -> Self {
        Stream {
            info: ConnectionInfoState::Connected(
                <TcpStream as HasConnectionInfo>::info(&stream).map(Into::into),
            ),
            inner: Braid::from(stream).into(),
        }
    }
}

#[cfg(feature = "stream")]
impl From<DuplexStream> for Stream {
    fn from(stream: DuplexStream) -> Self {
        Stream {
            info: ConnectionInfoState::Connected(
                <DuplexStream as HasConnectionInfo>::info(&stream).map(|_| BraidAddr::Duplex),
            ),
            inner: Braid::from(stream).into(),
        }
    }
}

#[cfg(feature = "stream")]
impl From<UnixStream> for Stream {
    fn from(stream: UnixStream) -> Self {
        Stream {
            info: ConnectionInfoState::Connected(stream.info().map(Into::into)),
            inner: Braid::from(stream).into(),
        }
    }
}

#[cfg(feature = "stream")]
impl From<Braid> for Stream {
    fn from(stream: Braid) -> Self {
        Stream {
            info: ConnectionInfoState::Connected(stream.info()),
            inner: stream.into(),
        }
    }
}

impl<IO> AsyncRead for Stream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    IO::Addr: Unpin,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.project().inner.poll_read(cx, buf)
    }
}

impl<IO> AsyncWrite for Stream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    IO::Addr: Unpin,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        self.project().inner.poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        self.project().inner.poll_shutdown(cx)
    }
}