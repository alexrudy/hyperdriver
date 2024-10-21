#[cfg(all(feature = "tls", feature = "stream"))]
use std::sync::Arc;
#[cfg(feature = "stream")]
use std::{io, net::SocketAddr};

use hyper::server::conn::{http1, http2};
use tower::make::Shared;

use crate::{bridge::rt::TokioExecutor, service::MakeServiceRef};

use super::conn::auto;
#[cfg(feature = "tls")]
use super::conn::tls::info::TlsConnectionInfoService;
#[cfg(feature = "stream")]
use super::conn::Acceptor;
use super::conn::MakeServiceConnectionInfoService;
use super::Accept;
use super::Server;

/// Indicates that the Server requires an acceptor.
#[derive(Debug, Clone, Copy, Default)]
pub struct NeedsAcceptor {
    _priv: (),
}

/// Indicates that the Server requires a protocol.
#[derive(Debug, Clone, Copy, Default)]
pub struct NeedsProtocol {
    _priv: (),
}

/// Indicates that the Server requires a service.
#[derive(Debug, Clone, Copy, Default)]
pub struct NeedsService {
    _priv: (),
}

/// Indicates that the Server requires an executor.
#[derive(Debug, Clone, Copy, Default)]
pub struct NeedsExecutor {
    _priv: (),
}

impl<P, S, B, E> Server<NeedsAcceptor, P, S, B, E> {
    /// Set the acceptor to use for incoming connections.
    pub fn with_acceptor<A>(self, acceptor: A) -> Server<A, P, S, B, E>
    where
        A: Accept,
    {
        Server {
            acceptor,
            make_service: self.make_service,
            protocol: self.protocol,
            executor: self.executor,
            body: self.body,
        }
    }

    #[cfg(feature = "stream")]
    /// Use an incoming stream of connections as the acceptor.
    ///
    /// This is a convenience method that constructs an `Acceptor` from the
    /// provided stream of connections. It works with `tokio::net::TcpListener`,
    /// `tokio::net::UnixListener`.
    pub fn with_incoming<I>(self, incoming: I) -> Server<Acceptor, P, S, B, E>
    where
        I: Into<Acceptor> + Into<crate::server::conn::AcceptorCore>,
    {
        self.with_acceptor(Acceptor::from(incoming))
    }

    #[cfg(feature = "stream")]
    /// Bind to the provided address and use it as the acceptor.
    pub async fn with_bind(
        self,
        addr: &SocketAddr,
    ) -> Result<Server<Acceptor, P, S, B, E>, io::Error> {
        Ok(self.with_acceptor(Acceptor::bind(addr).await?))
    }

    #[cfg(feature = "stream")]
    /// Use the provided listener as the acceptor.
    pub async fn with_listener(
        self,
        listener: tokio::net::TcpListener,
    ) -> Server<Acceptor, P, S, B, E> {
        self.with_acceptor(Acceptor::from(listener))
    }
}

impl<A, S, B, E> Server<A, NeedsProtocol, S, B, E> {
    /// Set the protocol to use for incoming connections.
    pub fn with_protocol<P>(self, protocol: P) -> Server<A, P, S, B, E> {
        Server {
            acceptor: self.acceptor,
            make_service: self.make_service,
            protocol,
            executor: self.executor,
            body: self.body,
        }
    }

    /// Use a protocol that automatically detects and selects
    /// between HTTP/1.1 and HTTP/2, by looking for the HTTP/2
    /// header in the initial bytes of the connection.
    pub fn with_auto_http(self) -> Server<A, auto::Builder, S, B, E> {
        self.with_protocol(auto::Builder::default())
    }

    /// Use HTTP/1.1 for all incoming connections.
    pub fn with_http1(self) -> Server<A, http1::Builder, S, B, E> {
        self.with_protocol(http1::Builder::new())
    }

    /// Use HTTP/2 for all incoming connections.
    pub fn with_http2(self) -> Server<A, http2::Builder<TokioExecutor>, S, B, E> {
        self.with_protocol(http2::Builder::new(TokioExecutor::new()))
    }
}

impl<A, P, B, E> Server<A, P, NeedsService, B, E> {
    /// Set the make service to use for handling incoming connections.
    ///
    /// A `MakeService` is a factory for creating `Service` instances. It is
    /// used to create a new `Service` for each incoming connection.
    ///
    /// If you have a service that is `Clone`, you can use `with_shared_service`
    /// to wrap it in a `Shared` and avoid constructing a new make service.
    pub fn with_make_service<S>(self, make_service: S) -> Server<A, P, S, B, E> {
        Server {
            acceptor: self.acceptor,
            make_service,
            protocol: self.protocol,
            executor: self.executor,
            body: self.body,
        }
    }

    /// Wrap a `Clone` service in a `Shared` to use as a make service.
    pub fn with_shared_service<S>(self, service: S) -> Server<A, P, Shared<S>, B, E> {
        Server {
            acceptor: self.acceptor,
            make_service: Shared::new(service),
            protocol: self.protocol,
            executor: self.executor,
            body: self.body,
        }
    }
}

impl<A, P, S, B, E> Server<A, P, S, B, E>
where
    S: MakeServiceRef<A::Conn, B>,
    A: Accept,
{
    /// Wrap the make service in a service that provides connection information.
    ///
    /// This will make `crate::info::ConnectionInfo<A>` available in the request
    /// extensions for each request handled by the generated service.
    pub fn with_connection_info(self) -> Server<A, P, MakeServiceConnectionInfoService<S>, B, E> {
        Server {
            acceptor: self.acceptor,
            make_service: MakeServiceConnectionInfoService::new(self.make_service),
            protocol: self.protocol,
            executor: self.executor,
            body: self.body,
        }
    }

    #[cfg(feature = "tls")]
    /// Wrap the make service in a service that provides TLS connection information.
    ///
    /// This will make `crate::info::TlsConnectionInfo` available in the request
    /// extensions for each request handled by the generated service.
    pub fn with_tls_connection_info(self) -> Server<A, P, TlsConnectionInfoService<S>, B, E> {
        Server {
            acceptor: self.acceptor,
            make_service: TlsConnectionInfoService::new(self.make_service),
            protocol: self.protocol,
            executor: self.executor,
            body: self.body,
        }
    }
}

#[cfg(all(feature = "tls", feature = "stream"))]
impl<P, S, B, E> Server<Acceptor, P, S, B, E> {
    /// Use the provided `rustls::ServerConfig` to configure TLS
    /// for incoming connections.
    pub fn with_tls<C>(self, config: C) -> Server<Acceptor, P, S, B, E>
    where
        C: Into<Arc<rustls::ServerConfig>>,
    {
        Server {
            acceptor: self.acceptor.with_tls(config.into()),
            make_service: self.make_service,
            protocol: self.protocol,
            executor: self.executor,
            body: self.body,
        }
    }
}

impl<A, P, S, B> Server<A, P, S, B, NeedsExecutor> {
    /// Set the executor for this server
    ///
    /// The executor is used to drive connections to completion asynchronously.
    pub fn with_executor<E>(self, executor: E) -> Server<A, P, S, B, E> {
        Server {
            acceptor: self.acceptor,
            make_service: self.make_service,
            protocol: self.protocol,
            executor,
            body: self.body,
        }
    }

    /// Use a tokio multi-threaded excutor via [tokio::task::spawn]
    ///
    /// This executor is a suitable default, but does require Send and 'static
    /// bounds in some places to allow futures to be moved between threads.
    ///
    /// Con
    pub fn with_tokio(self) -> Server<A, P, S, B, TokioExecutor> {
        self.with_executor(TokioExecutor::new())
    }
}

impl<A, P, S, B, E> Server<A, P, S, B, E> {
    /// Set the body to use for handling requests.
    ///
    /// Usually this method can be called with inferred
    /// types.
    pub fn with_body<B2>(self) -> Server<A, P, S, B2, E> {
        Server {
            acceptor: self.acceptor,
            make_service: self.make_service,
            protocol: self.protocol,
            executor: self.executor,
            body: Default::default(),
        }
    }
}
