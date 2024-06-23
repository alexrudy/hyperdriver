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

#[derive(Debug)]
pub struct Builder<A = NeedsAcceptor, P = NeedsProtocol, S = NeedsService, B = crate::Body> {
    acceptor: A,
    service: S,
    protocol: P,
    body: std::marker::PhantomData<fn(B) -> B>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NeedsAcceptor {
    _priv: (),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NeedsProtocol {
    _priv: (),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NeedsService {
    _priv: (),
}

impl Builder {
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
    pub fn with_incoming<I>(self, incoming: I) -> Builder<Acceptor, P, S, B>
    where
        I: Into<Acceptor> + Into<crate::server::conn::AcceptorCore>,
    {
        self.with_acceptor(Acceptor::from(incoming))
    }

    #[cfg(feature = "stream")]
    pub async fn with_bind(
        self,
        addr: &SocketAddr,
    ) -> Result<Builder<Acceptor, P, S, B>, io::Error> {
        Ok(self.with_acceptor(Acceptor::bind(addr).await?))
    }

    #[cfg(feature = "stream")]
    pub async fn with_listener(
        self,
        listener: tokio::net::TcpListener,
    ) -> Builder<Acceptor, P, S, B> {
        self.with_acceptor(Acceptor::from(listener))
    }
}

impl<A, S, B> Builder<A, NeedsProtocol, S, B> {
    pub fn with_protocol<P>(self, protocol: P) -> Builder<A, P, S, B> {
        Builder {
            acceptor: self.acceptor,
            service: self.service,
            protocol,
            body: self.body,
        }
    }

    pub fn with_auto_http(self) -> Builder<A, auto::Builder, S, B> {
        self.with_protocol(auto::Builder::default())
    }

    pub fn with_http1(self) -> Builder<A, http1::Builder, S, B> {
        self.with_protocol(http1::Builder::new())
    }

    pub fn with_http2(self) -> Builder<A, http2::Builder<TokioExecutor>, S, B> {
        self.with_protocol(http2::Builder::new(TokioExecutor::new()))
    }
}

impl<A, P, B> Builder<A, P, NeedsService, B> {
    pub fn with_make_service<S>(self, service: S) -> Builder<A, P, S, B> {
        Builder {
            acceptor: self.acceptor,
            service,
            protocol: self.protocol,
            body: self.body,
        }
    }

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
    pub fn with_connection_info(self) -> Builder<A, P, MakeServiceConnectionInfoService<S>, B> {
        Builder {
            acceptor: self.acceptor,
            service: MakeServiceConnectionInfoService::new(self.service),
            protocol: self.protocol,
            body: self.body,
        }
    }

    #[cfg(feature = "tls")]
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
    pub fn with_body<B2>(self) -> Builder<A, P, S, B2> {
        Builder {
            acceptor: self.acceptor,
            service: self.service,
            protocol: self.protocol,
            body: Default::default(),
        }
    }

    pub fn build(self) -> crate::Server<A, P, S, B> {
        crate::Server::new(self.acceptor, self.protocol, self.service)
    }
}
