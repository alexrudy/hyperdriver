use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use hyper::server::{accept::Accept, conn::AddrIncoming};
use pin_project::pin_project;
use tokio::net::UnixListener;

use crate::duplex::DuplexIncoming;
use crate::tls::server::acceptor::TlsAcceptor;

use super::Stream;

/// A stream of incoming connections.
///
/// This is a wrapper around hyper's `AddrIncoming`
/// and `TlsAcceptor` types, using enum-dispatch,
/// for compatibility with `Stream`.
#[derive(Debug)]
#[pin_project(project = AcceptorProj)]
pub enum Acceptor {
    Tcp(#[pin] AddrIncoming),
    Tls(#[pin] TlsAcceptor),
    Duplex(#[pin] DuplexIncoming),
    Unix(#[pin] UnixListener),
}

impl Acceptor {
    pub fn bind(addr: &SocketAddr) -> Result<Self, hyper::Error> {
        Ok(Self::Tcp(AddrIncoming::bind(addr)?))
    }
}

impl From<TlsAcceptor> for Acceptor {
    fn from(value: TlsAcceptor) -> Self {
        Self::Tls(value)
    }
}

impl From<AddrIncoming> for Acceptor {
    fn from(value: AddrIncoming) -> Self {
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
    ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
        let this = self.project();
        match this {
            AcceptorProj::Tcp(acceptor) => acceptor.poll_accept(cx).map(|stream| {
                stream.map(|stream| stream.and_then(|stream| stream.into_inner().try_into()))
            }),
            AcceptorProj::Tls(acceptor) => acceptor.poll_accept(cx).map_ok(|stream| stream.into()),
            AcceptorProj::Duplex(acceptor) => {
                acceptor.poll_accept(cx).map_ok(|stream| stream.into())
            }
            AcceptorProj::Unix(acceptor) => acceptor.poll_accept(cx).map(Some).map(|opt| {
                opt.map(|stream| stream.and_then(|(stream, _address)| stream.try_into()))
            }),
        }
    }
}

impl futures::Stream for Acceptor {
    type Item = Result<Stream, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        self.poll_accept(cx)
    }
}
