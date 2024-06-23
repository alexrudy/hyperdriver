#[cfg(all(feature = "tls", feature = "stream"))]
use std::sync::Arc;
#[cfg(feature = "stream")]
use std::{io, net::SocketAddr};

use hyper::server::conn::{http1, http2};
use tower::make::Shared;

use crate::{bridge::rt::TokioExecutor, service::MakeServiceRef};

use super::conn::auto;
#[cfg(feature = "stream")]
use super::conn::Acceptor;
use super::conn::MakeServiceConnectionInfoService;
use super::Accept;

#[cfg(feature = "tls")]
use super::conn::tls::info::TlsConnectionInfoService;

/// A builder for constructing a `Server`.
///
/// The builder uses the same set of generic types as the server to track the
/// required components.
///
/// To build a simple server, you can use the `with_shared_service` method:
/// ```rust
/// use hyperdriver::stream::duplex;
/// use hyperdriver::server::Builder;
/// use hyperdriver::Body;
/// use tower::service_fn;
///
/// #[derive(Debug)]
/// struct MyError;
///
/// impl std::fmt::Display for MyError {
///    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
///         f.write_str("MyError")
///   }
/// }
///
/// impl std::error::Error for MyError {}
///
/// # async fn example() {
/// let (_, incoming) = duplex::pair("server.test".parse().unwrap());
/// let server = Builder::new()
///     .with_acceptor(incoming)
///     .with_shared_service(service_fn(|req| async move {
///        Ok::<_, MyError>(http::Response::new(Body::empty()))
///    }))
///    .with_auto_http()
///    .build();
///
/// server.await.unwrap();
/// # }
/// ```
#[derive(Debug)]
pub struct Builder<A = NeedsAcceptor, P = NeedsProtocol, S = NeedsService, B = crate::Body> {
    acceptor: A,
    service: S,
    protocol: P,
    body: std::marker::PhantomData<fn(B) -> B>,
}

/// Indicates that the builder requires an acceptor.
#[derive(Debug, Clone, Copy, Default)]
pub struct NeedsAcceptor {
    _priv: (),
}

/// Indicates that the builder requires a protocol.
#[derive(Debug, Clone, Copy, Default)]
pub struct NeedsProtocol {
    _priv: (),
}

/// Indicates that the builder requires a service.
#[derive(Debug, Clone, Copy, Default)]
pub struct NeedsService {
    _priv: (),
}

impl Builder {
    /// Create a new builder in the initial configuration state.
    ///
    /// The acceptor, protocol, and service must be set before the builder can
    /// be used to construct a server.
    pub fn new() -> Builder<NeedsAcceptor, NeedsProtocol, NeedsService> {
        Builder {
            service: Default::default(),
            acceptor: Default::default(),
            protocol: Default::default(),
            body: Default::default(),
        }
    }
}

impl Default for Builder<NeedsAcceptor, NeedsProtocol, NeedsService, crate::Body> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P, S, B> Builder<NeedsAcceptor, P, S, B> {
    /// Set the acceptor to use for incoming connections.
    pub fn with_acceptor<A>(self, acceptor: A) -> Builder<A, P, S, B>
    where
        A: Accept,
    {
        Builder {
            acceptor,
            service: self.service,
            protocol: self.protocol,
            body: self.body,
        }
    }

    #[cfg(feature = "stream")]
    /// Use an incoming stream of connections as the acceptor.
    ///
    /// This is a convenience method that constructs an `Acceptor` from the
    /// provided stream of connections. It works with `tokio::net::TcpListener`,
    /// `tokio::net::UnixListener`.
    pub fn with_incoming<I>(self, incoming: I) -> Builder<Acceptor, P, S, B>
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
    ) -> Result<Builder<Acceptor, P, S, B>, io::Error> {
        Ok(self.with_acceptor(Acceptor::bind(addr).await?))
    }

    #[cfg(feature = "stream")]
    /// Use the provided listener as the acceptor.
    pub async fn with_listener(
        self,
        listener: tokio::net::TcpListener,
    ) -> Builder<Acceptor, P, S, B> {
        self.with_acceptor(Acceptor::from(listener))
    }
}

impl<A, S, B> Builder<A, NeedsProtocol, S, B> {
    /// Set the protocol to use for incoming connections.
    pub fn with_protocol<P>(self, protocol: P) -> Builder<A, P, S, B> {
        Builder {
            acceptor: self.acceptor,
            service: self.service,
            protocol,
            body: self.body,
        }
    }

    /// Use a protocol that automatically detects and selects
    /// between HTTP/1.1 and HTTP/2, by looking for the HTTP/2
    /// header in the initial bytes of the connection.
    pub fn with_auto_http(self) -> Builder<A, auto::Builder, S, B> {
        self.with_protocol(auto::Builder::default())
    }

    /// Use HTTP/1.1 for all incoming connections.
    pub fn with_http1(self) -> Builder<A, http1::Builder, S, B> {
        self.with_protocol(http1::Builder::new())
    }

    /// Use HTTP/2 for all incoming connections.
    pub fn with_http2(self) -> Builder<A, http2::Builder<TokioExecutor>, S, B> {
        self.with_protocol(http2::Builder::new(TokioExecutor::new()))
    }
}

impl<A, P, B> Builder<A, P, NeedsService, B> {
    /// Set the make service to use for handling incoming connections.
    ///
    /// A `MakeService` is a factory for creating `Service` instances. It is
    /// used to create a new `Service` for each incoming connection.
    ///
    /// If you have a service that is `Clone`, you can use `with_shared_service`
    /// to wrap it in a `Shared` and avoid constructing a new make service.
    pub fn with_make_service<S>(self, service: S) -> Builder<A, P, S, B> {
        Builder {
            acceptor: self.acceptor,
            service,
            protocol: self.protocol,
            body: self.body,
        }
    }

    /// Wrap a `Clone` service in a `Shared` to use as a make service.
    pub fn with_shared_service<S>(self, service: S) -> Builder<A, P, Shared<S>, B> {
        Builder {
            acceptor: self.acceptor,
            service: Shared::new(service),
            protocol: self.protocol,
            body: self.body,
        }
    }
}

impl<A, P, S, B> Builder<A, P, S, B>
where
    S: MakeServiceRef<A::Conn, B>,
    A: Accept,
{
    /// Wrap the make service in a service that provides connection information.
    ///
    /// This will make `crate::info::ConnectionInfo<A>` available in the request
    /// extensions for each request handled by the generated service.
    pub fn with_connection_info(self) -> Builder<A, P, MakeServiceConnectionInfoService<S>, B> {
        Builder {
            acceptor: self.acceptor,
            service: MakeServiceConnectionInfoService::new(self.service),
            protocol: self.protocol,
            body: self.body,
        }
    }

    #[cfg(feature = "tls")]
    /// Wrap the make service in a service that provides TLS connection information.
    ///
    /// This will make `crate::info::TlsConnectionInfo` available in the request
    /// extensions for each request handled by the generated service.
    pub fn with_tls_connection_info(self) -> Builder<A, P, TlsConnectionInfoService<S>, B> {
        Builder {
            acceptor: self.acceptor,
            service: TlsConnectionInfoService::new(self.service),
            protocol: self.protocol,
            body: self.body,
        }
    }
}

#[cfg(all(feature = "tls", feature = "stream"))]
impl<P, S, B> Builder<Acceptor, P, S, B> {
    /// Use the provided `rustls::ServerConfig` to configure TLS
    /// for incoming connections.
    pub fn with_tls<C>(self, config: C) -> Builder<Acceptor, P, S, B>
    where
        C: Into<Arc<rustls::ServerConfig>>,
    {
        Builder {
            acceptor: self.acceptor.with_tls(config.into()),
            service: self.service,
            protocol: self.protocol,
            body: self.body,
        }
    }
}

impl<A, P, S, B> Builder<A, P, S, B> {
    /// Set the body to use for handling requests.
    ///
    /// Usually this method can be called with inferred
    /// types.
    pub fn with_body<B2>(self) -> Builder<A, P, S, B2> {
        Builder {
            acceptor: self.acceptor,
            service: self.service,
            protocol: self.protocol,
            body: Default::default(),
        }
    }

    /// Construct the server.
    pub fn build(self) -> crate::Server<A, P, S, B> {
        crate::Server::new(self.acceptor, self.protocol, self.service)
    }
}
