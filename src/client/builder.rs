#[cfg(feature = "tls")]
use std::sync::Arc;
use std::time::Duration;

use http::HeaderValue;
use http_body::Body;
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
use super::conn::Transport;
use super::pool::PoolableConnection;

use super::ClientService;
use crate::client::conn::connection::ConnectionError;
#[cfg(feature = "tls")]
use crate::client::default_tls_config;
use crate::client::{conn::protocol::auto::HttpConnectionBuilder, Client};
use crate::info::HasConnectionInfo;
use crate::service::OptionLayerExt;
use crate::service::SharedService;
use crate::service::TimeoutLayer;

pub trait BuildProtocol<IO, B>
where
    IO: HasConnectionInfo,
{
    type Target: Protocol<IO, B>;
    fn build(self) -> Self::Target;
}

impl<P, IO, B> BuildProtocol<IO, B> for P
where
    P: Protocol<IO, B>,
    IO: HasConnectionInfo,
{
    type Target = P;
    fn build(self) -> Self::Target {
        self
    }
}

pub trait BuildTransport {
    type Target: Transport;
    fn build(self) -> Self::Target;
}

impl<T> BuildTransport for T
where
    T: Transport,
{
    type Target = T;
    fn build(self) -> Self::Target {
        self
    }
}

/// A builder for a client.
#[derive(Debug)]
pub struct Builder<T, P, RP = policy::Standard, S = Identity, BIn = crate::Body> {
    transport: T,
    protocol: P,
    builder: ServiceBuilder<S>,
    user_agent: Option<String>,
    redirect: Option<RP>,
    timeout: Option<Duration>,
    retries: Option<usize>,
    #[cfg(feature = "tls")]
    tls: Option<ClientConfig>,
    pool: Option<crate::client::pool::Config>,
    body: std::marker::PhantomData<fn(BIn) -> ()>,
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
            retries: None,
            #[cfg(feature = "tls")]
            tls: None,
            pool: None,
            body: std::marker::PhantomData,
        }
    }
}

impl Default
    for Builder<
        TcpTransportConfig,
        HttpConnectionBuilder<crate::Body>,
        policy::Standard,
        Identity,
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
            retries: Some(3),
            #[cfg(feature = "tls")]
            tls: Some(default_tls_config()),
            pool: Some(Default::default()),
            body: std::marker::PhantomData,
        }
    }
}

impl<T, P, RP, S, BIn> Builder<T, P, RP, S, BIn> {
    /// Use the provided TCP configuration.
    pub fn with_tcp(
        self,
        config: TcpTransportConfig,
    ) -> Builder<TcpTransportConfig, P, RP, S, BIn> {
        Builder {
            transport: config,
            protocol: self.protocol,
            builder: self.builder,
            user_agent: self.user_agent,
            redirect: self.redirect,
            timeout: self.timeout,
            retries: self.retries,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
        }
    }

    /// Provide a custom transport
    pub fn with_transport<T2>(self, transport: T2) -> Builder<T2, P, RP, S, BIn> {
        Builder {
            transport,
            protocol: self.protocol,
            builder: self.builder,
            user_agent: self.user_agent,
            redirect: self.redirect,
            timeout: self.timeout,
            retries: self.retries,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
        }
    }

    /// Get a mutable reference to the transport configuration
    pub fn transport(&mut self) -> &mut T {
        &mut self.transport
    }
}

#[cfg(feature = "tls")]
impl<T, P, RP, S, BIn> Builder<T, P, RP, S, BIn> {
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
impl<T, P, RP, S, BIn> Builder<T, P, RP, S, BIn> {
    /// Disable TLS
    pub fn without_tls(self) -> Self {
        self
    }
}

impl<T, P, RP, S, BIn> Builder<T, P, RP, S, BIn> {
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
}

impl<T, P, RP, S, BIn> Builder<T, P, RP, S, BIn> {
    /// Use the auto-HTTP Protocol
    pub fn with_auto_http(self) -> Builder<T, auto::HttpConnectionBuilder<BIn>, RP, S, BIn> {
        Builder {
            transport: self.transport,
            protocol: auto::HttpConnectionBuilder::default(),
            builder: self.builder,
            user_agent: self.user_agent,
            redirect: self.redirect,
            timeout: self.timeout,
            retries: self.retries,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
        }
    }

    /// Use the provided HTTP connection configuration.
    pub fn with_protocol<P2>(self, protocol: P2) -> Builder<T, P2, RP, S, BIn> {
        Builder {
            transport: self.transport,
            protocol,
            builder: self.builder,
            user_agent: self.user_agent,
            redirect: self.redirect,
            timeout: self.timeout,
            retries: self.retries,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
        }
    }

    /// HTTP connection configuration.
    pub fn protocol(&mut self) -> &mut P {
        &mut self.protocol
    }
}

impl<T, P, RP, S, BIn> Builder<T, P, RP, S, BIn> {
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

impl<T, P, RP, S, BIn> Builder<T, P, RP, S, BIn> {
    /// Set the redirect policy. See [`policy`] for more information.
    pub fn with_redirect_policy<RP2>(self, policy: RP2) -> Builder<T, P, RP2, S, BIn> {
        Builder {
            transport: self.transport,
            protocol: self.protocol,
            builder: self.builder,
            user_agent: self.user_agent,
            redirect: Some(policy),
            timeout: self.timeout,
            retries: self.retries,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
        }
    }

    /// Disable redirects.
    pub fn without_redirects(self) -> Builder<T, P, policy::Standard, S, BIn> {
        Builder {
            transport: self.transport,
            protocol: self.protocol,
            user_agent: self.user_agent,
            builder: self.builder,
            redirect: None,
            timeout: self.timeout,
            retries: self.retries,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
        }
    }

    /// Set the standard redirect policy. See [`policy::Standard`] for more information.
    pub fn with_standard_redirect_policy(self) -> Builder<T, P, policy::Standard, S, BIn> {
        Builder {
            transport: self.transport,
            protocol: self.protocol,
            builder: self.builder,
            user_agent: self.user_agent,
            redirect: Some(policy::Standard::default()),
            timeout: self.timeout,
            retries: self.retries,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
        }
    }

    /// Configured redirect policy.
    pub fn redirect_policy(&mut self) -> Option<&mut RP> {
        self.redirect.as_mut()
    }
}

impl<T, P, RP, S, BIn> Builder<T, P, RP, S, BIn> {
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

impl<T, P, RP, S, BIn> Builder<T, P, RP, S, BIn> {
    /// Set the number of retries for failed requests.
    pub fn with_retries(mut self, retries: usize) -> Self {
        self.retries = Some(retries);
        self
    }

    /// Get the number of retries for failed requests.
    pub fn retries(&self) -> Option<usize> {
        self.retries
    }

    /// Disable retries for failed requests.
    pub fn without_retries(mut self) -> Self {
        self.retries = None;
        self
    }
}

impl<T, P, RP, S, BIn> Builder<T, P, RP, S, BIn> {
    /// Add a layer to the service under construction
    pub fn layer<L>(self, layer: L) -> Builder<T, P, RP, Stack<L, S>, BIn> {
        Builder {
            transport: self.transport,
            protocol: self.protocol,
            builder: self.builder.layer(layer),
            user_agent: self.user_agent,
            redirect: self.redirect,
            timeout: self.timeout,
            retries: self.retries,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
        }
    }
}

impl<T, P, RP, S, BIn, BOut> Builder<T, P, RP, S, BIn>
where
    T: BuildTransport,
    <T as BuildTransport>::Target: Transport + Clone + Send + Sync + 'static,
    <<T as BuildTransport>::Target as Transport>::IO:
        tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    <<<T as BuildTransport>::Target as Transport>::IO as HasConnectionInfo>::Addr:
        Unpin + Clone + Send,
    P: BuildProtocol<
        super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
        BIn,
    >,
    <P as BuildProtocol<
        super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
        BIn,
    >>::Target: Protocol<
            super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
            BIn,
            Error = ConnectionError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <<P as BuildProtocol<
        super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
        BIn,
    >>::Target as Protocol<
        super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
        BIn,
    >>::Connection: Connection<BIn, ResBody = BOut> + PoolableConnection,

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
    <BIn as Body>::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    BOut: Body + Unpin + Send + 'static,
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
            .build()
            .with_optional_tls(self.tls.map(Arc::new));
        #[cfg(not(feature = "tls"))]
        let transport = self.transport.build().without_tls();

        let service = self
            .builder
            .layer(SharedService::layer())
            .optional(self.retries.map(|attempts| {
                tower::retry::RetryLayer::new(crate::service::Attempts::new(attempts))
            }))
            .optional(
                self.timeout
                    .map(|d| TimeoutLayer::new(|| super::Error::RequestTimeout, d)),
            )
            .optional(self.redirect.map(FollowRedirectLayer::with_policy))
            .layer(SetRequestHeaderLayer::if_not_present(
                http::header::USER_AGENT,
                user_agent,
            ))
            .service(ClientService {
                transport,
                protocol: self.protocol.build(),
                pool: self.pool.map(super::pool::Pool::new),
                _body: std::marker::PhantomData,
            });

        SharedService::new(service)
    }
}

impl<T, P, RP, S> Builder<T, P, RP, S, crate::Body>
where
    T: BuildTransport,
    <T as BuildTransport>::Target: Transport + Clone + Send + Sync + 'static,
    <<T as BuildTransport>::Target as Transport>::IO:
        tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    <<<T as BuildTransport>::Target as Transport>::IO as HasConnectionInfo>::Addr:
        Unpin + Clone + Send,
    P: BuildProtocol<
        super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
        crate::Body,
    >,
    <P as BuildProtocol<
        super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
        crate::Body,
    >>::Target: Protocol<
            super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
            crate::Body,
            Error = ConnectionError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <<P as BuildProtocol<
        super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
        crate::Body,
    >>::Target as Protocol<
        super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
        crate::Body,
    >>::Connection: Connection<crate::Body, ResBody = hyper::body::Incoming> + PoolableConnection,

    RP: policy::Policy<crate::Body, super::Error> + Clone + Send + Sync + 'static,
    S: tower::Layer<
        SharedService<
            http::Request<crate::Body>,
            http::Response<hyper::body::Incoming>,
            super::Error,
        >,
    >,
    S::Service: tower::Service<
            http::Request<crate::Body>,
            Response = http::Response<hyper::body::Incoming>,
            Error = super::Error,
        > + Clone
        + Send
        + Sync
        + 'static,
    <S::Service as tower::Service<http::Request<crate::Body>>>::Future: Send + 'static,
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
        let _ = Builder::default().build();
    }
}
