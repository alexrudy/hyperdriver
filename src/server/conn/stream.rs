//! Server side of the Braid stream
//!
//! The server and client are differentiated for TLS support, but otherwise,
//! TCP and Duplex streams are the same whether they are server or client.

#[cfg(feature = "tls")]
use std::io;
use std::task::{Context, Poll};
use std::{future::Future, pin::Pin};

use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};
#[cfg(feature = "stream")]
use tokio::net::{TcpStream, UnixStream};

use crate::info::{ConnectionInfo, HasConnectionInfo};
#[cfg(feature = "stream")]
use crate::stream::duplex::DuplexStream;
#[cfg(feature = "stream")]
use crate::stream::Braid;

#[cfg(feature = "tls")]
use crate::info::tls::TlsConnectionInfoReciever;

#[cfg(feature = "tls")]
use crate::stream::TlsBraid;

#[cfg(feature = "tls")]
use crate::server::conn::tls::TlsStream;
#[cfg(feature = "tls")]
use crate::stream::tls::TlsHandshakeInfo;
#[cfg(feature = "tls")]
use crate::stream::tls::TlsHandshakeStream;

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

/// Extension trait for Accept
pub trait AcceptExt: Accept {
    /// Wrap the acceptor in a future that resolves to a single connection.
    ///
    /// For example, to accept only a single duplex connection, you can do:
    /// ```
    /// # use hyperdriver::server::conn::AcceptExt;
    /// # use hyperdriver::stream::duplex;
    /// # async fn demo_accept() {
    /// let (client, acceptor) = duplex::pair();
    ///
    /// let (client_conn, server_conn) = tokio::try_join!(client.connect(1024), acceptor.accept()).unwrap();
    /// # }
    fn accept(self) -> AcceptOne<Self>
    where
        Self: Sized,
    {
        AcceptOne::new(self)
    }
}

impl<A> AcceptExt for A where A: Accept {}

/// A future that resolves to a single connection
#[derive(Debug)]
#[pin_project]
pub struct AcceptOne<A> {
    #[pin]
    inner: A,
}

impl<A> AcceptOne<A> {
    fn new(inner: A) -> Self {
        AcceptOne { inner }
    }
}

impl<A> Future for AcceptOne<A>
where
    A: Accept,
    A::Conn: HasConnectionInfo,
    <<A as Accept>::Conn as HasConnectionInfo>::Addr: Clone + Unpin + Send + Sync + 'static,
{
    type Output = Result<A::Conn, A::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        self.project().inner.poll_accept(cx)
    }
}

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
    tls: TlsConnectionInfoReciever,

    #[cfg(feature = "tls")]
    #[pin]
    inner: TlsBraid<TlsStream<IO>, IO>,

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
    tls: TlsConnectionInfoReciever,

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
            info: inner.info(),

            #[cfg(feature = "tls")]
            tls: TlsConnectionInfoReciever::empty(),

            #[cfg(feature = "tls")]
            inner: TlsBraid::NoTls(inner),

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
            TlsBraid::Tls(stream) => stream.poll_handshake(cx),
            TlsBraid::NoTls(_) => Poll::Ready(Ok(())),
        }
    }
}

#[cfg(feature = "tls")]
impl<IO> TlsHandshakeInfo for Stream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin,
    IO::Addr: Send + Unpin,
{
    fn recv(&self) -> TlsConnectionInfoReciever {
        self.inner.recv()
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
impl<IO> From<TlsStream<IO>> for Stream<IO>
where
    IO: HasConnectionInfo,
    IO::Addr: Clone,
{
    fn from(stream: TlsStream<IO>) -> Self {
        Stream {
            info: stream.info(),
            tls: stream.rx.clone(),
            inner: TlsBraid::Tls(stream),
        }
    }
}

#[cfg(feature = "stream")]
impl From<TcpStream> for Stream {
    fn from(stream: TcpStream) -> Self {
        Stream {
            info: stream.info().map(Into::into),
            #[cfg(feature = "tls")]
            tls: TlsConnectionInfoReciever::empty(),
            inner: Braid::from(stream).into(),
        }
    }
}

#[cfg(feature = "stream")]
impl From<DuplexStream> for Stream {
    fn from(stream: DuplexStream) -> Self {
        Stream {
            info: stream.info().map(Into::into),
            #[cfg(feature = "tls")]
            tls: TlsConnectionInfoReciever::empty(),
            inner: Braid::from(stream).into(),
        }
    }
}

#[cfg(feature = "stream")]
impl From<UnixStream> for Stream {
    fn from(stream: UnixStream) -> Self {
        Stream {
            info: stream.info().map(Into::into),
            #[cfg(feature = "tls")]
            tls: TlsConnectionInfoReciever::empty(),
            inner: Braid::from(stream).into(),
        }
    }
}

#[cfg(feature = "stream")]
impl From<Braid> for Stream {
    fn from(stream: Braid) -> Self {
        Stream {
            info: stream.info(),
            #[cfg(feature = "tls")]
            tls: TlsConnectionInfoReciever::empty(),
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
