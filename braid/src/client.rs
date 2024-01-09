//! Client side of the Braid stream
//!
//! The server and client are differentiated for TLS support, but otherwise,
//! TCP and Duplex streams are the same whether they are server or client.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UnixStream};

use crate::core::{Braid, BraidCore};
use crate::duplex::DuplexStream;
use crate::info::Connection as _;
use crate::tls::client::TlsStream;

/// A stream which can handle multiple different underlying transports, and TLS
/// through a unified type.
///
/// This is the client side of the Braid stream.
#[derive(Debug)]
#[pin_project]
pub struct Stream {
    #[pin]
    inner: Braid<TlsStream<BraidCore>>,
}

impl Stream {
    /// Connect to a server via TCP at the given address.
    ///
    /// For other connection methods/types, use the appropriate `From` impl.
    pub async fn connect(addr: impl Into<SocketAddr>) -> std::io::Result<Self> {
        let stream = TcpStream::connect(addr.into()).await?;
        Ok(stream.into())
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
            Braid::NoTls(core) => core,
            Braid::Tls(_) => panic!("Stream::tls called twice"),
        };

        Stream {
            inner: Braid::Tls(TlsStream::new(core, domain, config)),
        }
    }

    /// Finish the TLS handshake.
    ///
    /// This is a no-op if TLS is not enabled. When TLS is enabled, this method
    /// will drive the connection asynchronosly allowing you to wait for the TLS
    /// handshake to complete. If this method is not called, the TLS handshake
    /// will be completed the first time the connection is used.
    pub async fn finish_handshake(&mut self) -> io::Result<()> {
        match self.inner {
            Braid::Tls(ref mut stream) => stream.finish_handshake().await,
            _ => Ok(()),
        }
    }

    /// Get information about the connection.
    ///
    /// This method is async because TLS information isn't available until the handshake
    /// is complete. This method will not return until the handshake is complete.
    pub async fn info(&self) -> io::Result<crate::info::ConnectionInfo> {
        match self.inner {
            Braid::Tls(ref stream) => stream.info().await,
            Braid::NoTls(ref stream) => Ok(stream.info()),
        }
    }
}

impl From<TcpStream> for Stream {
    fn from(stream: TcpStream) -> Self {
        Stream {
            inner: stream.into(),
        }
    }
}

impl From<DuplexStream> for Stream {
    fn from(stream: DuplexStream) -> Self {
        Stream {
            inner: stream.into(),
        }
    }
}

impl From<UnixStream> for Stream {
    fn from(stream: UnixStream) -> Self {
        Stream {
            inner: stream.into(),
        }
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.project().inner.poll_read(cx, buf)
    }
}

impl AsyncWrite for Stream {
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
