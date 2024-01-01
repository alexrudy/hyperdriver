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

/// Dispatching wrapper for potential stream connection types for clients
#[derive(Debug)]
#[pin_project]
pub struct Stream {
    #[pin]
    inner: Braid<TlsStream<BraidCore>>,
}

impl Stream {
    pub async fn connect(addr: impl Into<SocketAddr>) -> std::io::Result<Self> {
        let stream = TcpStream::connect(addr.into()).await?;
        Ok(stream.into())
    }

    pub fn tls(self, domain: &str, config: Arc<rustls::ClientConfig>) -> Self {
        let core = match self.inner {
            Braid::NoTls(core) => core,
            Braid::Tls(_) => panic!("Stream::tls called twice"),
        };

        Stream {
            inner: Braid::Tls(TlsStream::new(core, domain, config)),
        }
    }

    pub async fn finish_handshake(&mut self) -> io::Result<()> {
        match self.inner {
            Braid::Tls(ref mut stream) => stream.finish_handshake().await,
            _ => Ok(()),
        }
    }

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
