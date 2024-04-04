//! Client side of the Braid stream
//!
//! The server and client are differentiated for TLS support, but otherwise,
//! TCP and Duplex streams are the same whether they are server or client.

use std::future::poll_fn;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UnixStream};

use crate::stream::core::{Braid, TlsBraid};
use crate::stream::duplex::DuplexStream;
use crate::stream::info::Connection;
use crate::stream::tls::client::ClientTlsStream;
use crate::stream::tls::TlsHandshakeStream as _;

/// A stream which can handle multiple different underlying transports, and TLS
/// through a unified type.
///
/// This is the client side of the Braid stream.
#[derive(Debug)]
#[pin_project]
pub struct Stream<IO = Braid> {
    #[pin]
    inner: TlsBraid<ClientTlsStream<IO>, IO>,
}

impl Stream {
    /// Connect to a server via TCP at the given address.
    ///
    /// For other connection methods/types, use the appropriate `From` impl.
    pub async fn connect(addr: impl Into<SocketAddr>) -> std::io::Result<Self> {
        let stream = TcpStream::connect(addr.into()).await?;
        Ok(stream.into())
    }
}

impl<IO> Stream<IO>
where
    IO: Connection + AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    /// Create a new client stream from an existing connection.
    pub fn new(inner: IO) -> Self {
        Stream {
            inner: TlsBraid::NoTls(inner),
        }
    }

    /// Add TLS to the underlying stream.
    ///
    /// # Panics
    /// TLS can only be added once. If this is called twice, it will panic.
    ///
    /// # Arguments
    ///
    /// * `domain` - The domain name to connect to. This is used for SNI.
    /// * `config` - The TLS client configuration to use.
    pub fn tls(self, domain: &str, config: Arc<rustls::ClientConfig>) -> Self {
        let core = match self.inner {
            crate::stream::core::TlsBraid::NoTls(core) => core,
            crate::stream::core::TlsBraid::Tls(_) => panic!("Stream::tls called twice"),
        };

        Stream {
            inner: crate::stream::core::TlsBraid::Tls(ClientTlsStream::new(core, domain, config)),
        }
    }

    /// Finish the TLS handshake.
    ///
    /// This is a no-op if TLS is not enabled. When TLS is enabled, this method
    /// will drive the connection asynchronosly allowing you to wait for the TLS
    /// handshake to complete. If this method is not called, the TLS handshake
    /// will be completed the first time the connection is used.
    pub async fn finish_handshake(&mut self) -> io::Result<()> {
        poll_fn(|cx| self.inner.poll_handshake(cx)).await
    }

    /// Get information about the connection.
    ///
    /// This method is async because TLS information isn't available until the handshake
    /// is complete. This method will not return until the handshake is complete.
    pub async fn info(&self) -> io::Result<crate::stream::info::ConnectionInfo> {
        match self.inner {
            crate::stream::core::TlsBraid::Tls(ref stream) => stream.info().await,
            crate::stream::core::TlsBraid::NoTls(ref stream) => Ok(stream.info()),
        }
    }
}

impl From<TcpStream> for Stream {
    fn from(stream: TcpStream) -> Self {
        Stream {
            inner: Braid::from(stream).into(),
        }
    }
}

impl From<DuplexStream> for Stream {
    fn from(stream: DuplexStream) -> Self {
        Stream {
            inner: Braid::from(stream).into(),
        }
    }
}

impl From<UnixStream> for Stream {
    fn from(stream: UnixStream) -> Self {
        Stream {
            inner: Braid::from(stream).into(),
        }
    }
}

impl<IO> AsyncRead for Stream<IO>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.project().inner.poll_read(cx, buf)
    }
}

impl<IO> AsyncWrite for Stream<IO>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        self.project().inner.poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        self.project().inner.poll_shutdown(cx)
    }
}
