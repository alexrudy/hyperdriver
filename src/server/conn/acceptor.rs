//! Accept incoming connections for Braid streams.

#[cfg(feature = "stream")]
use std::io;
#[cfg(feature = "stream")]
use std::net::SocketAddr;
use std::pin::Pin;
#[cfg(feature = "tls")]
use std::sync::Arc;
use std::task::{Context, Poll};

use pin_project::pin_project;
#[cfg(feature = "tls")]
use rustls::ServerConfig;
#[cfg(feature = "stream")]
use tokio::net::{TcpListener, UnixListener};

use super::Accept;
use super::Stream;
use crate::info::HasConnectionInfo;
#[cfg(feature = "tls")]
use crate::server::conn::tls::TlsAcceptor as RawTlsAcceptor;
#[cfg(feature = "stream")]
use crate::stream::{duplex::DuplexIncoming, Braid};

/// Accept incoming connections for streams which might
/// be wrapped in TLS.
#[cfg_attr(feature = "tls", doc = "Use [`Acceptor::with_tls`] to enable TLS.")]
#[cfg(feature = "stream")]
#[derive(Debug)]
#[pin_project]
pub struct Acceptor<A = AcceptorCore> {
    #[pin]
    inner: AcceptorInner<A>,
}

/// Accept incoming connections for streams which might
/// be wrapped in TLS. Use [`Acceptor::tls`] to enable TLS.
#[cfg(not(feature = "stream"))]
#[derive(Debug)]
#[pin_project]
pub struct Acceptor<A> {
    #[pin]
    inner: AcceptorInner<A>,
}

impl<A> Acceptor<A> {
    /// Create a new acceptor from the given acceptor.
    pub fn new(accept: A) -> Self {
        Acceptor {
            inner: AcceptorInner::NoTls(accept),
        }
    }
}

#[derive(Debug)]
#[pin_project(project = AcceptorInnerProj)]
enum AcceptorInner<A> {
    NoTls(#[pin] A),

    #[cfg(feature = "tls")]
    Tls(#[pin] RawTlsAcceptor<A>),
}

#[cfg(feature = "stream")]
#[derive(Debug)]
#[pin_project(project = AcceptorCoreProj)]
enum AcceptorCoreInner {
    /// A TCP listener.
    Tcp(#[pin] TcpListener),

    /// A duplex listener.
    Duplex(#[pin] DuplexIncoming),

    /// A Unix listener.
    Unix(#[pin] UnixListener),
}

/// A stream of incoming connections.
///
/// This is a wrapper around hyper's `AddrIncoming`
/// and `TlsAcceptor` types, using enum-dispatch,
/// for compatibility with `Stream`.
#[cfg(feature = "stream")]
#[derive(Debug)]
#[pin_project]
pub struct AcceptorCore {
    #[pin]
    inner: AcceptorCoreInner,
}

#[cfg(feature = "stream")]
impl Acceptor {
    /// Bind to a TCP socket address, returning the acceptor
    /// which will product incoming connections as [`Stream`]s.
    ///
    /// For other connections, see the `From` impls.
    pub async fn bind(addr: &SocketAddr) -> Result<Self, io::Error> {
        Ok(TcpListener::bind(addr).await?.into())
    }
}

#[cfg(feature = "tls")]
impl<A> Acceptor<A> {
    /// Convert this acceptor to support TLS on top of the underlying
    /// transport.
    ///
    /// # Panics
    /// TLS can only be added once. If this is called twice, it will panic.
    ///
    /// # Arguments
    ///
    /// * `config` - The TLS server configuration to use.
    pub fn with_tls(self, config: Arc<ServerConfig>) -> Self {
        let core = match self.inner {
            AcceptorInner::NoTls(core) => core,
            AcceptorInner::Tls(_) => panic!("Acceptor::tls called twice"),
        };

        Acceptor {
            inner: AcceptorInner::Tls(RawTlsAcceptor::new(config, core)),
        }
    }
}

#[cfg(feature = "stream")]
impl From<TcpListener> for AcceptorCore {
    fn from(value: TcpListener) -> Self {
        AcceptorCore {
            inner: AcceptorCoreInner::Tcp(value),
        }
    }
}

#[cfg(feature = "stream")]
impl From<DuplexIncoming> for AcceptorCore {
    fn from(value: DuplexIncoming) -> Self {
        AcceptorCore {
            inner: AcceptorCoreInner::Duplex(value),
        }
    }
}

#[cfg(feature = "stream")]
impl From<UnixListener> for AcceptorCore {
    fn from(value: UnixListener) -> Self {
        AcceptorCore {
            inner: AcceptorCoreInner::Unix(value),
        }
    }
}

#[cfg(feature = "stream")]
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

#[cfg(feature = "stream")]
impl Accept for AcceptorCore {
    type Conn = Braid;
    type Error = io::Error;

    fn poll_accept(
        self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<Result<Self::Conn, Self::Error>> {
        match self.project().inner.project() {
            AcceptorCoreProj::Tcp(acceptor) => acceptor
                .poll_accept(cx)
                .map(|stream| stream.map(Braid::from)),
            AcceptorCoreProj::Duplex(acceptor) => {
                acceptor.poll_accept(cx).map_ok(|stream| stream.into())
            }
            AcceptorCoreProj::Unix(acceptor) => acceptor
                .poll_accept(cx)
                .map(|stream| stream.map(Braid::from)),
        }
    }
}

impl<A> Accept for Acceptor<A>
where
    A: Accept,
    A::Conn: HasConnectionInfo,
    <<A as Accept>::Conn as HasConnectionInfo>::Addr: Clone + Unpin + Send + Sync + 'static,
{
    type Conn = Stream<A::Conn>;
    type Error = A::Error;

    fn poll_accept(
        self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<Result<Self::Conn, Self::Error>> {
        match self.project().inner.project() {
            AcceptorInnerProj::NoTls(acceptor) => {
                acceptor.poll_accept(cx).map(|r| r.map(Stream::new))
            }

            #[cfg(feature = "tls")]
            AcceptorInnerProj::Tls(acceptor) => {
                acceptor.poll_accept(cx).map(|r| r.map(|s| s.into()))
            }
        }
    }
}

impl<A> futures_core::Stream for Acceptor<A>
where
    A: Accept,
    A::Conn: HasConnectionInfo,
    <<A as Accept>::Conn as HasConnectionInfo>::Addr: Clone + Unpin + Send + Sync + 'static,
{
    type Item = Result<Stream<A::Conn>, A::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        self.poll_accept(cx).map(Some)
    }
}
