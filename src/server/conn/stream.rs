//! Server side of the Braid stream
//!
//! The server and client are differentiated for TLS support, but otherwise,
//! TCP and Duplex streams are the same whether they are server or client.

use std::fmt;
#[cfg(feature = "tls")]
use std::io;
use std::task::{Context, Poll};

#[cfg(feature = "stream")]
use crate::stream::Braid;
use chateau::info::ConnectionInfo;
use chateau::info::HasConnectionInfo;
use chateau::info::HasTlsConnectionInfo;
#[cfg(feature = "stream")]
use chateau::stream::duplex::DuplexStream;
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

#[cfg(feature = "tls")]
use chateau::stream::tls::OptTlsStream;

#[cfg(feature = "tls")]
use chateau::server::conn::tls::TlsStream;
#[cfg(feature = "tls")]
use chateau::stream::tls::TlsHandshakeStream;

/// Dispatching wrapper for potential stream connection types for servers
#[cfg(feature = "stream")]
#[derive(Debug)]
#[pin_project]
pub struct Stream<IO = Braid>
where
    IO: HasConnectionInfo,
{
    info: ConnectionInfo<IO::Addr>,

    #[cfg(feature = "tls")]
    #[pin]
    inner: OptTlsStream<TlsStream<IO>, IO>,

    #[cfg(not(feature = "tls"))]
    #[pin]
    inner: IO,
}

/// Dispatching wrapper for potential stream connection types for servers
#[cfg(not(feature = "stream"))]
#[derive(Debug)]
#[pin_project]
pub struct Stream<IO>
where
    IO: HasConnectionInfo,
{
    info: ConnectionInfo<IO::Addr>,

    #[cfg(feature = "tls")]
    #[pin]
    inner: OptTlsStream<TlsStream<IO>, IO>,

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
            info: inner.info(),

            #[cfg(feature = "tls")]
            inner: OptTlsStream::NoTls(inner),

            #[cfg(not(feature = "tls"))]
            inner,
        }
    }
}

#[cfg(feature = "tls")]
impl<IO> TlsHandshakeStream for Stream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin,
    IO::Addr: Send + Unpin,
{
    fn poll_handshake(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match &mut self.inner {
            OptTlsStream::Tls(stream) => stream.poll_handshake(cx),
            OptTlsStream::NoTls(_) => Poll::Ready(Ok(())),
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
        self.info.clone()
    }
}

#[cfg(feature = "tls")]
impl<IO, A> HasTlsConnectionInfo for Stream<IO>
where
    IO: HasConnectionInfo<Addr = A> + HasTlsConnectionInfo,
    A: fmt::Debug + fmt::Display + Clone + Send + 'static,
    TlsStream<IO>: HasConnectionInfo<Addr = A> + HasTlsConnectionInfo,
{
    fn tls_info(&self) -> Option<&chateau::info::TlsConnectionInfo> {
        self.inner.tls_info()
    }
}

#[cfg(not(feature = "tls"))]
impl<IO, A> HasTlsConnectionInfo for Stream<IO>
where
    IO: HasConnectionInfo<Addr = A> + HasTlsConnectionInfo,
    A: fmt::Debug + fmt::Display + Clone + Send + 'static,
{
    fn tls_info(&self) -> Option<&chateau::info::TlsConnectionInfo> {
        self.inner.tls_info()
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
            info: stream.info(),
            inner: OptTlsStream::Tls(stream),
        }
    }
}

#[cfg(feature = "stream")]
#[allow(clippy::useless_conversion)]
impl From<DuplexStream> for Stream {
    fn from(stream: DuplexStream) -> Self {
        Stream {
            info: stream.info().map(Into::into),
            inner: Braid::from(stream).into(),
        }
    }
}

#[cfg(feature = "stream")]
#[allow(clippy::useless_conversion)]
impl From<Braid> for Stream {
    fn from(stream: Braid) -> Self {
        Stream {
            info: stream.info(),
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
