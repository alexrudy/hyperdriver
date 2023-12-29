//! Server side of the Braid stream
//!
//! The server and client are differentiated for TLS support, but otherwise,
//! TCP and Duplex streams are the same whether they are server or client.

use std::io;

use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UnixStream};

use crate::core::Braid;
use crate::duplex::DuplexStream;
use crate::info::{ConnectionInfo, SocketAddr};
use crate::tls::server::info::TlsConnectionInfoReciever;
use crate::tls::server::TlsStream;

pub mod acceptor;
mod connector;

pub use connector::{Connection, StartConnectionInfoLayer, StartConnectionInfoService};

#[derive(Debug, Clone)]
enum ConnectionInfoState {
    Handshake(TlsConnectionInfoReciever),
    Connected(ConnectionInfo),
}

impl ConnectionInfoState {
    async fn recv(&self) -> ConnectionInfo {
        match self {
            ConnectionInfoState::Handshake(rx) => rx.recv().await,
            ConnectionInfoState::Connected(info) => info.clone(),
        }
    }
}

pub trait Accept {
    type Conn: AsyncRead + AsyncWrite + Send + Unpin + 'static;
    type Error: Into<Box<dyn std::error::Error + Send + Sync>>;

    fn poll_accept(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::Conn, Self::Error>>;
}

/// Dispatching wrapper for potential stream connection types for clients
#[derive(Debug)]
#[pin_project]
pub struct Stream {
    info: ConnectionInfoState,

    #[pin]
    inner: Braid<TlsStream>,
}

impl Stream {
    pub async fn info(&self) -> ConnectionInfo {
        match &self.info {
            ConnectionInfoState::Handshake(rx) => rx.recv().await,
            ConnectionInfoState::Connected(info) => info.clone(),
        }
    }

    pub fn remote_addr(&self) -> Option<&SocketAddr> {
        match &self.info {
            ConnectionInfoState::Handshake(rx) => Some(rx.remote_addr()),
            ConnectionInfoState::Connected(info) => info.remote_addr(),
        }
    }

    pub fn tcp(stream: TcpStream, remote_addr: SocketAddr) -> Self {
        Stream {
            info: ConnectionInfoState::Connected(ConnectionInfo::Tcp(
                crate::info::TcpConnectionInfo::new(
                    stream.local_addr().expect("tcp stream local addr").into(),
                    remote_addr,
                    None,
                ),
            )),
            inner: Braid::Tcp(stream),
        }
    }
}

impl From<TlsStream> for Stream {
    fn from(stream: TlsStream) -> Self {
        Stream {
            info: ConnectionInfoState::Handshake(stream.rx.clone()),
            inner: Braid::Tls(stream),
        }
    }
}

impl From<DuplexStream> for Stream {
    fn from(stream: DuplexStream) -> Self {
        Stream {
            info: ConnectionInfoState::Connected((&stream).into()),
            inner: Braid::Duplex(stream),
        }
    }
}

impl TryFrom<UnixStream> for Stream {
    type Error = io::Error;
    fn try_from(stream: UnixStream) -> Result<Self, Self::Error> {
        Ok(Stream {
            info: ConnectionInfoState::Connected((&stream).try_into()?),
            inner: Braid::Unix(stream),
        })
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
