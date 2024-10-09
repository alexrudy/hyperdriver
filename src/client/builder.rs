#[cfg(feature = "tls")]
use std::sync::Arc;
use std::time::Duration;

use http::HeaderValue;
use http_body::Body;
use hyper::rt::Executor;
#[cfg(feature = "tls")]
use rustls::ClientConfig;
use tower::layer::util::{Identity, Stack};
use tower::ServiceBuilder;
use tower_http::follow_redirect::policy;
use tower_http::follow_redirect::FollowRedirectLayer;
use tower_http::set_header::SetRequestHeaderLayer;

use super::conn::protocol::auto;
use super::conn::transport::tcp::TcpTransportConfig;
use super::conn::transport::TransportExt;
use super::conn::Connection;
use super::conn::Protocol;
use super::conn::TlsTransport;
use super::conn::Transport;
use super::pool::{DelayedCheckout, PoolableConnection, PoolableStream};
use super::ConnectionPoolLayer;
use crate::bridge::rt::TokioExecutor;
use crate::service::client::WhenReady;
use crate::service::RequestExecutor;
use crate::service::{Http1ChecksLayer, Http2ChecksLayer, SetHostHeaderLayer};
use crate::BoxError;

use crate::client::conn::connection::ConnectionError;
#[cfg(feature = "tls")]
use crate::client::default_tls_config;
use crate::client::{conn::protocol::auto::HttpConnectionBuilder, Client};
use crate::info::HasConnectionInfo;
use crate::service::IncomingResponseLayer;
use crate::service::OptionLayerExt;
use crate::service::SharedService;
use crate::service::TimeoutLayer;

pub trait BuildProtocol<IO, B>
where
    IO: HasConnectionInfo,
{
    type Target: Protocol<IO, B>;
    fn build_protocol(self) -> Self::Target;
}

impl<P, IO, B> BuildProtocol<IO, B> for P
where
    P: Protocol<IO, B>,
    IO: HasConnectionInfo,
{
    type Target = P;
    fn build_protocol(self) -> Self::Target {
        self
    }
}

pub trait BuildTransport {
    type Target: Transport;
    fn build_transport(self) -> Self::Target;
}

impl<T> BuildTransport for T
where
    T: Transport,
{
    type Target = T;
    fn build_transport(self) -> Self::Target {
        self
    }
}

/// A builder for a client.
#[derive(Debug)]
pub struct Builder<
    T,
    P,
    RP = policy::Standard,
    S = Identity,
    BIn = crate::Body,
    BOut = crate::Body,
    E = TokioExecutor,
> {
    transport: T,
    protocol: P,
    builder: ServiceBuilder<S>,
    user_agent: Option<String>,
    redirect: Option<RP>,
    timeout: Option<Duration>,
    #[cfg(feature = "tls")]
    tls: Option<ClientConfig>,
    pool: Option<crate::client::pool::Config>,
    body: std::marker::PhantomData<fn(BIn) -> BOut>,
    executor: E,
}

impl Builder<(), (), policy::Standard> {
    /// Create a new, empty builder
    pub fn new() -> Self {
        Self {
            transport: (),
            protocol: (),
            builder: ServiceBuilder::new(),
            user_agent: None,
            redirect: None,
            timeout: None,
            #[cfg(feature = "tls")]
            tls: None,
            pool: None,
            body: std::marker::PhantomData,
            executor: TokioExecutor::new(),
        }
    }
}

impl Default
    for Builder<
        TcpTransportConfig,
        HttpConnectionBuilder<crate::Body, TokioExecutor>,
        policy::Standard,
        Identity,
        crate::Body,
        crate::Body,
    >
{
    fn default() -> Self {
        Self {
            transport: Default::default(),
            protocol: Default::default(),
            builder: ServiceBuilder::new(),
            user_agent: None,
            redirect: Some(policy::Standard::default()),
            timeout: Some(Duration::from_secs(30)),
            #[cfg(feature = "tls")]
            tls: Some(default_tls_config()),
            pool: Some(Default::default()),
            body: std::marker::PhantomData,
            executor: TokioExecutor::new(),
        }
    }
}

impl<T, P, RP, S, BIn, BOut, E> Builder<T, P, RP, S, BIn, BOut, E> {
    /// Use the provided TCP configuration.
    pub fn with_tcp(
        self,
        config: TcpTransportConfig,
    ) -> Builder<TcpTransportConfig, P, RP, S, BIn, BOut, E> {
        Builder {
            transport: config,
            protocol: self.protocol,
            builder: self.builder,
            user_agent: self.user_agent,
            redirect: self.redirect,
            timeout: self.timeout,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
            executor: self.executor,
        }
    }

    /// Provide a custom transport
    pub fn with_transport<T2>(self, transport: T2) -> Builder<T2, P, RP, S, BIn, BOut, E> {
        Builder {
            transport,
            protocol: self.protocol,
            builder: self.builder,
            user_agent: self.user_agent,
            redirect: self.redirect,
            timeout: self.timeout,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
            executor: self.executor,
        }
    }

    /// Get a mutable reference to the transport configuration
    pub fn transport(&mut self) -> &mut T {
        &mut self.transport
    }
}

#[cfg(feature = "tls")]
impl<T, P, RP, S, BIn, BOut, E> Builder<T, P, RP, S, BIn, BOut, E> {
    /// Disable TLS
    pub fn without_tls(mut self) -> Self {
        self.tls = None;
        self
    }

    /// Use the provided TLS configuration.
    pub fn with_tls(mut self, config: ClientConfig) -> Self {
        self.tls = Some(config);
        self
    }

    /// Use the default TLS configuration with native root certificates.
    pub fn with_default_tls(mut self) -> Self {
        self.tls = Some(default_tls_config());
        self
    }

    /// TLS configuration.
    pub fn tls(&mut self) -> &mut Option<ClientConfig> {
        &mut self.tls
    }
}

#[cfg(not(feature = "tls"))]
impl<T, P, RP, S, BIn, BOut, E> Builder<T, P, RP, S, BIn, BOut, E> {
    /// Disable TLS
    pub fn without_tls(self) -> Self {
        self
    }
}

impl<T, P, RP, S, BIn, BOut, E> Builder<T, P, RP, S, BIn, BOut, E> {
    /// Connection pool configuration.
    pub fn pool(&mut self) -> Option<&mut crate::client::pool::Config> {
        self.pool.as_mut()
    }

    /// Use the provided connection pool configuration.
    pub fn with_pool(mut self, pool: crate::client::pool::Config) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Configure the default pool settings
    pub fn with_default_pool(mut self) -> Self {
        self.pool = Some(Default::default());
        self
    }

    /// Disable connection pooling.
    pub fn without_pool(mut self) -> Self {
        self.pool = None;
        self
    }

    /// Get a mutable reference to the executor
    pub fn executor(&mut self) -> &mut E {
        &mut self.executor
    }

    /// Use a custom executor
    pub fn with_executor<E2>(self, executor: E2) -> Builder<T, P, RP, S, BIn, BOut, E2> {
        Builder {
            transport: self.transport,
            protocol: self.protocol,
            builder: self.builder,
            user_agent: self.user_agent,
            redirect: self.redirect,
            timeout: self.timeout,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
            executor,
        }
    }
}

impl<T, P, RP, S, BIn, BOut, E> Builder<T, P, RP, S, BIn, BOut, E> {
    /// Use the auto-HTTP Protocol
    pub fn with_auto_http(
        self,
    ) -> Builder<T, auto::HttpConnectionBuilder<BIn, E>, RP, S, BIn, BOut, E>
    where
        E: Clone + Default,
    {
        Builder {
            transport: self.transport,
            protocol: auto::HttpConnectionBuilder::default(),
            builder: self.builder,
            user_agent: self.user_agent,
            redirect: self.redirect,
            timeout: self.timeout,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
            executor: self.executor,
        }
    }

    /// Use the provided HTTP connection configuration.
    pub fn with_protocol<P2>(self, protocol: P2) -> Builder<T, P2, RP, S, BIn, BOut, E> {
        Builder {
            transport: self.transport,
            protocol,
            builder: self.builder,
            user_agent: self.user_agent,
            redirect: self.redirect,
            timeout: self.timeout,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
            executor: self.executor,
        }
    }

    /// HTTP connection configuration.
    pub fn protocol(&mut self) -> &mut P {
        &mut self.protocol
    }
}

impl<T, P, RP, S, BIn, BOut, E> Builder<T, P, RP, S, BIn, BOut, E> {
    /// Set the User-Agent header.
    pub fn with_user_agent(mut self, user_agent: String) -> Self {
        self.user_agent = Some(user_agent);
        self
    }

    /// Get the user agent currently configured
    pub fn user_agent(&self) -> Option<&str> {
        self.user_agent.as_deref()
    }
}

impl<T, P, RP, S, BIn, BOut, E> Builder<T, P, RP, S, BIn, BOut, E> {
    /// Set the redirect policy. See [`policy`] for more information.
    pub fn with_redirect_policy<RP2>(self, policy: RP2) -> Builder<T, P, RP2, S, BIn, BOut, E> {
        Builder {
            transport: self.transport,
            protocol: self.protocol,
            builder: self.builder,
            user_agent: self.user_agent,
            redirect: Some(policy),
            timeout: self.timeout,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
            executor: self.executor,
        }
    }

    /// Disable redirects.
    pub fn without_redirects(self) -> Builder<T, P, policy::Standard, S, BIn, BOut, E> {
        Builder {
            transport: self.transport,
            protocol: self.protocol,
            user_agent: self.user_agent,
            builder: self.builder,
            redirect: None,
            timeout: self.timeout,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
            executor: self.executor,
        }
    }

    /// Set the standard redirect policy. See [`policy::Standard`] for more information.
    pub fn with_standard_redirect_policy(self) -> Builder<T, P, policy::Standard, S, BIn, BOut, E> {
        Builder {
            transport: self.transport,
            protocol: self.protocol,
            builder: self.builder,
            user_agent: self.user_agent,
            redirect: Some(policy::Standard::default()),
            timeout: self.timeout,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
            executor: self.executor,
        }
    }

    /// Configured redirect policy.
    pub fn redirect_policy(&mut self) -> Option<&mut RP> {
        self.redirect.as_mut()
    }
}

impl<T, P, RP, S, BIn, BOut, E> Builder<T, P, RP, S, BIn, BOut, E> {
    /// Set the timeout for requests.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Get the timeout for requests.
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Disable request timeouts.
    pub fn without_timeout(mut self) -> Self {
        self.timeout = None;
        self
    }

    /// Set the timeout for requests with an Option.
    pub fn with_optional_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }
}

impl<T, P, RP, S, BIn, BOut, E> Builder<T, P, RP, S, BIn, BOut, E> {
    /// Add a layer to the service under construction
    pub fn layer<L>(self, layer: L) -> Builder<T, P, RP, Stack<L, S>, BIn, BOut, E> {
        Builder {
            transport: self.transport,
            protocol: self.protocol,
            builder: self.builder.layer(layer),
            user_agent: self.user_agent,
            redirect: self.redirect,
            timeout: self.timeout,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
            executor: self.executor,
        }
    }
}

impl<TB, T, PB, P, IO, RP, S, BIn, BOut, E> Builder<TB, PB, RP, S, BIn, BOut, E>
where
    TB: BuildTransport<Target = T>,
    T: Transport<IO = IO> + Clone + Send + Sync + 'static,
    <T as Transport>::Future: Send + 'static,
    IO: HasConnectionInfo
        + PoolableStream
        + tokio::io::AsyncRead
        + tokio::io::AsyncWrite
        + Unpin
        + Send
        + 'static,
    <IO as HasConnectionInfo>::Addr: Unpin + Clone + Send,
    PB: BuildProtocol<super::conn::stream::Stream<IO>, BIn, Target = P>,
    P: Protocol<super::conn::stream::Stream<IO>, BIn, Error = ConnectionError>
        + Clone
        + Send
        + Sync
        + 'static,
    <P as Protocol<super::conn::stream::Stream<IO>, BIn>>::Connection:
        Connection<BIn, ResBody = hyper::body::Incoming> + PoolableConnection + Send,
    <P as Protocol<super::conn::stream::Stream<IO>, BIn>>::Error:
        std::error::Error + Send + Sync + 'static,
    <P as Protocol<super::conn::stream::Stream<IO>, BIn>>::Future: Send + 'static,
    RP: policy::Policy<BIn, super::Error> + Clone + Send + Sync + 'static,
    S: tower::Layer<SharedService<http::Request<BIn>, http::Response<BOut>, super::Error>>,
    S::Service: tower::Service<http::Request<BIn>, Response = http::Response<BOut>, Error = super::Error>
        + Clone
        + Send
        + Sync
        + 'static,
    <S::Service as tower::Service<http::Request<BIn>>>::Future: Send + 'static,
    BIn: Default + Body + Unpin + Send + 'static,
    <BIn as Body>::Data: Send,
    <BIn as Body>::Error: Into<BoxError>,
    BOut: From<hyper::body::Incoming> + Body + Unpin + Send + 'static,
    E: Executor<DelayedCheckout<TlsTransport<T>, P, BIn>>
        + Executor<WhenReady<P::Connection, BIn>>
        + Clone
        + Send
        + Sync
        + 'static,
{
    /// Build a client service with the configured layers
    pub fn build_service(
        self,
    ) -> SharedService<http::Request<BIn>, http::Response<BOut>, super::Error> {
        let user_agent = if let Some(ua) = self.user_agent {
            HeaderValue::from_str(&ua).expect("user-agent should be a valid http header")
        } else {
            HeaderValue::from_static(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ))
        };

        #[cfg(feature = "tls")]
        let transport = self
            .transport
            .build_transport()
            .with_optional_tls(self.tls.map(Arc::new));
        #[cfg(not(feature = "tls"))]
        let transport = self.transport.build_transport().without_tls();

        let service = self
            .builder
            .layer(SharedService::layer())
            .optional(
                self.timeout
                    .map(|d| TimeoutLayer::new(|| super::Error::RequestTimeout, d)),
            )
            .optional(self.redirect.map(FollowRedirectLayer::with_policy))
            .layer(SetRequestHeaderLayer::if_not_present(
                http::header::USER_AGENT,
                user_agent,
            ))
            .layer(IncomingResponseLayer::new())
            .layer(
                ConnectionPoolLayer::new(
                    transport,
                    self.protocol.build_protocol(),
                    self.executor.clone(),
                )
                .with_optional_pool(self.pool),
            )
            .layer(SetHostHeaderLayer::new())
            .layer(Http2ChecksLayer::new())
            .layer(Http1ChecksLayer::new())
            .service(RequestExecutor::new(self.executor));

        SharedService::new(service)
    }
}

impl<TB, T, PB, P, IO, RP, S, E> Builder<TB, PB, RP, S, crate::Body, crate::Body, E>
where
    TB: BuildTransport<Target = T>,
    T: Transport<IO = IO> + Clone + Send + Sync + 'static,
    <T as Transport>::Future: Send + 'static,
    IO: HasConnectionInfo
        + PoolableStream
        + tokio::io::AsyncRead
        + tokio::io::AsyncWrite
        + Unpin
        + Send
        + 'static,
    <IO as HasConnectionInfo>::Addr: Unpin + Clone + Send,
    PB: BuildProtocol<super::conn::stream::Stream<IO>, crate::Body, Target = P>,
    P: Protocol<super::conn::stream::Stream<IO>, crate::Body, Error = ConnectionError>
        + Clone
        + Send
        + Sync
        + 'static,
    <P as Protocol<super::conn::stream::Stream<IO>, crate::Body>>::Connection:
        Connection<crate::Body, ResBody = hyper::body::Incoming> + PoolableConnection + Send,
    <P as Protocol<super::conn::stream::Stream<IO>, crate::Body>>::Future: Send + 'static,
    <P as Protocol<super::conn::stream::Stream<IO>, crate::Body>>::Error:
        std::error::Error + Send + Sync + 'static,
    RP: policy::Policy<crate::Body, super::Error> + Clone + Send + Sync + 'static,
    S: tower::Layer<
        SharedService<http::Request<crate::Body>, http::Response<crate::Body>, super::Error>,
    >,
    S::Service: tower::Service<
            http::Request<crate::Body>,
            Response = http::Response<crate::Body>,
            Error = super::Error,
        > + Clone
        + Send
        + Sync
        + 'static,
    <S::Service as tower::Service<http::Request<crate::Body>>>::Future: Send + 'static,
    E: Executor<DelayedCheckout<TlsTransport<T>, P, crate::Body>>
        + Executor<WhenReady<P::Connection, crate::Body>>
        + Clone
        + Send
        + Sync
        + 'static,
{
    /// Build the client.
    pub fn build(self) -> Client {
        Client::new_from_service(self.build_service())
    }
}

#[cfg(test)]
mod tests {
    use super::Builder;

    #[test]
    fn build_default_compiles() {
        #[cfg(feature = "tls")]
        {
            crate::fixtures::tls_install_default();
        }

        let _ = Builder::default().build();
    }
}
