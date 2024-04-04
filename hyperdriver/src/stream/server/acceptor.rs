//! Accept incoming connections for Braid streams.

use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use pin_project::pin_project;
use rustls::ServerConfig;
use tokio::net::{TcpListener, UnixListener};

use super::Accept;
use super::Stream;
use crate::stream::tls::server::TlsAcceptor as RawTlsAcceptor;
use crate::stream::{core::BraidCore, duplex::DuplexIncoming};

/// Accept incoming connections for Braid streams.
#[derive(Debug)]
#[pin_project]
pub struct Acceptor {
    #[pin]
    inner: AcceptorInner,
}

#[derive(Debug)]
#[pin_project(project = AcceptorInnerProj)]
enum AcceptorInner {
    NoTls(#[pin] AcceptorCore),
    Tls(#[pin] RawTlsAcceptor<AcceptorCore>),
}

/// A stream of incoming connections.
///
/// This is a wrapper around hyper's `AddrIncoming`
/// and `TlsAcceptor` types, using enum-dispatch,
/// for compatibility with `Stream`.
#[derive(Debug)]
#[pin_project(project = AcceptorProj)]
enum AcceptorCore {
    Tcp(#[pin] TcpListener),
    Duplex(#[pin] DuplexIncoming),
    Unix(#[pin] UnixListener),
}

impl Acceptor {
    /// Bind to a TCP socket address, returning the acceptor
    /// which will product incoming connections as [`Stream`]s.
    ///
    /// For other connections, see the `From` impls.
    pub async fn bind(addr: &SocketAddr) -> Result<Self, io::Error> {
        Ok(TcpListener::bind(addr).await?.into())
    }

    /// Convert this acceptor to support TLS on top of the underlying
    /// transport.
    ///
    /// # Panics
    /// TLS can only be added once. If this is called twice, it will panic.
    ///
    /// # Arguments
    ///
    /// * `config` - The TLS server configuration to use.
    pub fn tls(self, config: Arc<ServerConfig>) -> Self {
        let core = match self.inner {
            AcceptorInner::NoTls(core) => core,
            AcceptorInner::Tls(_) => panic!("Acceptor::tls called twice"),
        };

        Acceptor {
            inner: AcceptorInner::Tls(RawTlsAcceptor::new(config, core)),
        }
    }
}

impl From<TcpListener> for AcceptorCore {
    fn from(value: TcpListener) -> Self {
        AcceptorCore::Tcp(value)
    }
}

impl From<DuplexIncoming> for AcceptorCore {
    fn from(value: DuplexIncoming) -> Self {
        AcceptorCore::Duplex(value)
    }
}

impl From<UnixListener> for AcceptorCore {
    fn from(value: UnixListener) -> Self {
        AcceptorCore::Unix(value)
    }
}

impl<T> From<T> for Acceptor
where
    T: Into<AcceptorCore>,
{
    fn from(value: T) -> Self {
        Acceptor {
            inner: AcceptorInner::NoTls(value.into()),
        }
    }
}

impl Accept for AcceptorCore {
    type Conn = BraidCore;
    type Error = io::Error;

    fn poll_accept(
        self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<Result<Self::Conn, Self::Error>> {
        match self.project() {
            AcceptorProj::Tcp(acceptor) => acceptor
                .poll_accept(cx)
                .map(|stream| stream.map(|(stream, _)| stream.into())),
            AcceptorProj::Duplex(acceptor) => {
                acceptor.poll_accept(cx).map_ok(|stream| stream.into())
            }
            AcceptorProj::Unix(acceptor) => acceptor
                .poll_accept(cx)
                .map(|stream| stream.map(|(stream, _address)| stream.into())),
        }
    }
}

impl Accept for Acceptor {
    type Conn = Stream;
    type Error = io::Error;

    fn poll_accept(
        self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<Result<Self::Conn, Self::Error>> {
        match self.project().inner.project() {
            AcceptorInnerProj::NoTls(acceptor) => {
                acceptor.poll_accept(cx).map(|r| r.map(Stream::from))
            }
            AcceptorInnerProj::Tls(acceptor) => {
                acceptor.poll_accept(cx).map(|r| r.map(|s| s.into()))
            }
        }
    }
}

impl futures_core::Stream for Acceptor {
    type Item = Result<Stream, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        self.poll_accept(cx).map(Some)
    }
}
