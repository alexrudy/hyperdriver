#[cfg(feature = "stream")]
use std::{io, net::SocketAddr};

#[cfg(feature = "tls")]
use chateau::server::conn::tls::info::TlsConnectionInfoService;
use chateau::server::NeedsAcceptor;
use chateau::server::NeedsProtocol;
use chateau::services::MakeServiceRef;
use hyper::server::conn::{http1, http2};

use crate::bridge::rt::TokioExecutor;

use super::conn::auto;
#[cfg(feature = "stream")]
use super::conn::Acceptor;
use super::conn::Http1Builder;
use super::conn::Http2Builder;
use super::conn::MakeServiceConnectionInfoService;
use super::Accept;
use super::Server;

/// Extension trait to allow additional methods for building servers from common listeners.
#[allow(async_fn_in_trait)]
pub trait ServerAcceptorExt<P, S, B, E> {
    #[cfg(feature = "stream")]
    /// Use an incoming stream of connections as the acceptor.
    ///
    /// This is a convenience method that constructs an `Acceptor` from the
    /// provided stream of connections. It works with `tokio::net::TcpListener`,
    /// `tokio::net::UnixListener`.
    fn with_incoming<I>(self, incoming: I) -> Server<Acceptor, P, S, B, E>
    where
        I: Into<Acceptor> + Into<crate::server::conn::AcceptorCore>;

    #[cfg(feature = "stream")]
    /// Bind to the provided address and use it as the acceptor.
    async fn with_bind(self, addr: &SocketAddr) -> Result<Server<Acceptor, P, S, B, E>, io::Error>;

    #[cfg(feature = "stream")]
    /// Use the provided listener as the acceptor.
    async fn with_listener(self, listener: tokio::net::TcpListener)
        -> Server<Acceptor, P, S, B, E>;
}

impl<P, S, B, E> ServerAcceptorExt<P, S, B, E> for Server<NeedsAcceptor, P, S, B, E> {
    #[cfg(feature = "stream")]
    fn with_incoming<I>(self, incoming: I) -> Server<Acceptor, P, S, B, E>
    where
        I: Into<Acceptor> + Into<crate::server::conn::AcceptorCore>,
    {
        self.with_acceptor(Acceptor::from(incoming))
    }

    #[cfg(feature = "stream")]
    async fn with_bind(self, addr: &SocketAddr) -> Result<Server<Acceptor, P, S, B, E>, io::Error> {
        Ok(self.with_acceptor(Acceptor::bind(addr).await?))
    }

    #[cfg(feature = "stream")]
    async fn with_listener(
        self,
        listener: tokio::net::TcpListener,
    ) -> Server<Acceptor, P, S, B, E> {
        self.with_acceptor(Acceptor::from(listener))
    }
}

/// An extension to add protocol helpers to the server builder.
pub trait ServerProtocolExt<A, S, B, E> {
    /// Use a protocol that automatically detects and selects
    /// between HTTP/1.1 and HTTP/2, by looking for the HTTP/2
    /// header in the initial bytes of the connection.
    fn with_auto_http(self) -> Server<A, auto::Builder, S, B, E>;

    /// Use HTTP/1.1 for all incoming connections.
    fn with_http1(self) -> Server<A, Http1Builder, S, B, E>;

    /// Use HTTP/2 for all incoming connections.
    fn with_http2(self) -> Server<A, Http2Builder<TokioExecutor>, S, B, E>;
}
impl<A, S, B, E> ServerProtocolExt<A, S, B, E> for Server<A, NeedsProtocol, S, B, E> {
    fn with_auto_http(self) -> Server<A, auto::Builder, S, B, E> {
        self.with_protocol(auto::Builder::default())
    }

    fn with_http1(self) -> Server<A, Http1Builder, S, B, E> {
        self.with_protocol(http1::Builder::new().into())
    }

    fn with_http2(self) -> Server<A, Http2Builder<TokioExecutor>, S, B, E> {
        self.with_protocol(http2::Builder::new(TokioExecutor::new()).into())
    }
}

/// Extension trait for exposing connection info in the server.
pub trait ServerConnectionInfoExt<A, P, S, B, E> {
    /// Wrap the make service in a service that provides connection information.
    ///
    /// This will make `crate::info::ConnectionInfo<A>` available in the request
    /// extensions for each request handled by the generated service.
    fn with_connection_info(self) -> Server<A, P, MakeServiceConnectionInfoService<S>, B, E>;

    /// Wrap the make service in a service that provides TLS connection information.
    ///
    /// This will make `crate::info::TlsConnectionInfo` available in the request
    /// extensions for each request handled by the generated service.
    #[cfg(feature = "tls")]
    fn with_tls_connection_info(self) -> Server<A, P, TlsConnectionInfoService<S>, B, E>;
}

impl<A, P, S, B, E> ServerConnectionInfoExt<A, P, S, B, E> for Server<A, P, S, B, E>
where
    S: MakeServiceRef<A::Connection, B>,
    A: Accept,
{
    fn with_connection_info(self) -> Server<A, P, MakeServiceConnectionInfoService<S>, B, E> {
        self.map_service(MakeServiceConnectionInfoService::new)
    }

    #[cfg(feature = "tls")]
    fn with_tls_connection_info(self) -> Server<A, P, TlsConnectionInfoService<S>, B, E> {
        self.map_service(TlsConnectionInfoService::new)
    }
}
