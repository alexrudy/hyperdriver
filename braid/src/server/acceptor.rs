use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use pin_project::pin_project;
use tokio::net::{TcpListener, UnixListener};

use super::Accept;
use super::Stream;
use crate::duplex::DuplexIncoming;
use crate::tls::server::TlsAcceptor;

/// A stream of incoming connections.
///
/// This is a wrapper around hyper's `AddrIncoming`
/// and `TlsAcceptor` types, using enum-dispatch,
/// for compatibility with `Stream`.
#[derive(Debug)]
#[pin_project(project = AcceptorProj)]
pub enum Acceptor {
    Tcp(#[pin] TcpListener),
    Tls(#[pin] TlsAcceptor),
    Duplex(#[pin] DuplexIncoming),
    Unix(#[pin] UnixListener),
}

impl Acceptor {
    pub async fn bind(addr: &SocketAddr) -> Result<Self, io::Error> {
        Ok(Self::Tcp(TcpListener::bind(addr).await?))
    }
}

impl From<TlsAcceptor> for Acceptor {
    fn from(value: TlsAcceptor) -> Self {
        Self::Tls(value)
    }
}

impl From<TcpListener> for Acceptor {
    fn from(value: TcpListener) -> Self {
        Self::Tcp(value)
    }
}

impl From<DuplexIncoming> for Acceptor {
    fn from(value: DuplexIncoming) -> Self {
        Self::Duplex(value)
    }
}

impl From<UnixListener> for Acceptor {
    fn from(value: UnixListener) -> Self {
        Self::Unix(value)
    }
}

impl Accept for Acceptor {
    type Conn = Stream;
    type Error = io::Error;

    fn poll_accept(
        self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<Result<Self::Conn, Self::Error>> {
        match self.project() {
            AcceptorProj::Tcp(acceptor) => acceptor.poll_accept(cx).map(|stream| {
                stream.map(|(stream, remote_addr)| Stream::tcp(stream, remote_addr.into()))
            }),
            AcceptorProj::Tls(acceptor) => acceptor.poll_accept(cx).map_ok(|stream| stream.into()),
            AcceptorProj::Duplex(acceptor) => {
                acceptor.poll_accept(cx).map_ok(|stream| stream.into())
            }
            AcceptorProj::Unix(acceptor) => acceptor
                .poll_accept(cx)
                .map(|stream| stream.and_then(|(stream, _address)| stream.try_into())),
        }
    }
}

impl futures_core::Stream for Acceptor {
    type Item = Result<Stream, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        self.poll_accept(cx).map(Some)
    }
}
