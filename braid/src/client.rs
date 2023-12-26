//! Client side of the Braid stream
//!
//! The server and client are differentiated for TLS support, but otherwise,
//! TCP and Duplex streams are the same whether they are server or client.

use hyper::client::connect::Connection;
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UnixStream};

use crate::core::Braid;
use crate::duplex::DuplexStream;
use crate::tls::client::TlsStream;

/// Dispatching wrapper for potential stream connection types for clients
#[derive(Debug)]
#[pin_project]
pub struct Stream {
    #[pin]
    inner: Braid<TlsStream>,
}

impl From<TcpStream> for Stream {
    fn from(stream: TcpStream) -> Self {
        Stream {
            inner: Braid::Tcp(stream),
        }
    }
}

impl From<TlsStream> for Stream {
    fn from(stream: TlsStream) -> Self {
        Stream {
            inner: Braid::Tls(stream),
        }
    }
}

impl From<DuplexStream> for Stream {
    fn from(stream: DuplexStream) -> Self {
        Stream {
            inner: Braid::Duplex(stream),
        }
    }
}

impl From<UnixStream> for Stream {
    fn from(stream: UnixStream) -> Self {
        Stream {
            inner: Braid::Unix(stream),
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

//TODO: This implementation of Connected is probably wrong
// it doesn't actually respect anything about the connection states
impl Connection for Stream {
    fn connected(&self) -> hyper::client::connect::Connected {
        match &self.inner {
            Braid::Tcp(stream) => stream.connected(),
            Braid::Duplex(stream) => {
                if stream.info().protocol.is_http2() {
                    hyper::client::connect::Connected::new().negotiated_h2()
                } else {
                    hyper::client::connect::Connected::new()
                }
            }
            Braid::Tls(stream) => stream.connected(),
            Braid::Unix(_) => hyper::client::connect::Connected::new().negotiated_h2(),
        }
    }
}
