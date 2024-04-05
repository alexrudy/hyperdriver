//! Client side of the Braid stream
//!
//! The server and client are differentiated for TLS support, but otherwise,
//! TCP and Duplex streams are the same whether they are server or client.

#[cfg(feature = "tls")]
use std::future::poll_fn;

#[cfg(feature = "tls")]
use std::io;

use std::net::SocketAddr;

#[cfg(feature = "tls")]
use std::sync::Arc;

use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UnixStream};

use crate::stream::core::Braid;

#[cfg(feature = "tls")]
use crate::stream::core::TlsBraid;

use crate::stream::duplex::DuplexStream;
use crate::stream::info::HasConnectionInfo;

#[cfg(feature = "tls")]
use crate::stream::tls::client::ClientTlsStream;

#[cfg(feature = "tls")]
use crate::stream::tls::TlsHandshakeStream as _;

/// A stream which can handle multiple different underlying transports, and TLS
/// through a unified type.
///
/// This is the client side of the Braid stream.
#[derive(Debug)]
#[pin_project]
pub struct Stream<IO = Braid>
where
    IO: HasConnectionInfo,
{
    #[cfg(feature = "tls")]
    #[pin]
    inner: TlsBraid<ClientTlsStream<IO>, IO>,

    #[cfg(not(feature = "tls"))]
    #[pin]
    inner: IO,
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
    IO: HasConnectionInfo,
{
    /// Create a new client stream from an existing connection.
    pub fn new(inner: IO) -> Self {
        Stream {
            #[cfg(feature = "tls")]
            inner: TlsBraid::NoTls(inner),

            #[cfg(not(feature = "tls"))]
            inner,
        }
    }
}

#[cfg(feature = "tls")]
impl<IO> Stream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    IO::Addr: Clone,
{
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
}

#[cfg(feature = "tls")]
impl<IO> Stream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    IO::Addr: Unpin + Clone,
{
    /// Finish the TLS handshake.
    ///
    /// This is a no-op if TLS is not enabled. When TLS is enabled, this method
    /// will drive the connection asynchronosly allowing you to wait for the TLS
    /// handshake to complete. If this method is not called, the TLS handshake
    /// will be completed the first time the connection is used.
    pub async fn finish_handshake(&mut self) -> io::Result<()> {
        poll_fn(|cx| self.inner.poll_handshake(cx)).await
    }
}

impl<IO> HasConnectionInfo for Stream<IO>
where
    IO: HasConnectionInfo,
    IO::Addr: Unpin + Clone,
{
    type Addr = IO::Addr;

    /// Get information about the connection.
    ///
    /// This method is async because TLS information isn't available until the handshake
    /// is complete. This method will not return until the handshake is complete.
    fn info(&self) -> crate::stream::info::ConnectionInfo<IO::Addr> {
        #[cfg(feature = "tls")]
        match self.inner {
            crate::stream::core::TlsBraid::Tls(ref stream) => stream.info(),
            crate::stream::core::TlsBraid::NoTls(ref stream) => stream.info(),
        }

        #[cfg(not(feature = "tls"))]
        self.inner.info()
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
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    IO::Addr: Unpin,
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
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    IO::Addr: Unpin,
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
