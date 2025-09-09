use std::sync::Arc;
use std::time::Duration;

use chateau::client::conn::dns::Resolver;
use chateau::client::conn::service::ClientExecutorService;
#[cfg(feature = "tls")]
use chateau::client::conn::stream::tls::TlsStream;
use http::HeaderValue;
use http_body::Body;
#[cfg(feature = "tls")]
use rustls::ClientConfig;
use tower::layer::util::{Identity, Stack};
use tower::ServiceBuilder;
use tower_http::follow_redirect::policy;
use tower_http::follow_redirect::FollowRedirectLayer;
use tower_http::set_header::SetRequestHeaderLayer;

use super::conn::dns::GaiResolver;
#[cfg(feature = "tls")]
use super::conn::dns::TlsResolver;
use super::conn::protocol::auto;
use crate::client::UriKey;
use crate::service::{Http1ChecksLayer, Http2ChecksLayer, HttpConnection, SetHostHeaderLayer};
use crate::BoxError;
use chateau::client::conn::transport::tcp::{TcpTransport, TcpTransportConfig};
use chateau::client::conn::Connection;
use chateau::client::conn::Protocol;
#[cfg(feature = "tls")]
use chateau::client::conn::TlsTransport;
use chateau::client::conn::Transport;
use chateau::client::pool::{PoolableConnection, PoolableStream};
use chateau::client::ConnectionPoolLayer;

#[cfg(feature = "tls")]
use crate::client::default_tls_config;
use crate::client::{conn::protocol::auto::HttpConnectionBuilder, Client};
use crate::service::IncomingResponseLayer;
use crate::service::OptionLayerExt;
use crate::service::TimeoutLayer;
use chateau::info::HasConnectionInfo;
use chateau::services::SharedService;

/// A builder for a client.
#[derive(Debug)]
pub struct Builder<
    D,
    T,
    P,
    RP = policy::Standard,
    S = Identity,
    BIn = crate::Body,
    BOut = crate::Body,
> {
    resolver: D,
    transport: T,
    protocol: P,
    builder: ServiceBuilder<S>,
    user_agent: Option<String>,
    redirect: Option<RP>,
    timeout: Option<Duration>,
    #[cfg(feature = "tls")]
    tls: Option<ClientConfig>,
    pool: Option<chateau::client::pool::Config>,
    body: std::marker::PhantomData<fn(BIn) -> BOut>,
}

impl Builder<(), (), (), policy::Standard> {
    /// Create a new, empty builder
    pub fn new() -> Self {
        Self {
            resolver: (),
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
        }
    }
}

impl Default
    for Builder<
        GaiResolver,
        TcpTransport,
        HttpConnectionBuilder<crate::Body>,
        policy::Standard,
        Identity,
        crate::Body,
        crate::Body,
    >
{
    fn default() -> Self {
        Self {
            resolver: Default::default(),
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
        }
    }
}

impl<D, T, P, RP, S, BIn, BOut> Builder<D, T, P, RP, S, BIn, BOut> {
    /// Use the provided TCP configuration.
    pub fn with_tcp(
        self,
        config: TcpTransportConfig,
    ) -> Builder<D, TcpTransport, P, RP, S, BIn, BOut> {
        Builder {
            resolver: self.resolver,
            transport: TcpTransport::new(Arc::new(config)),
            protocol: self.protocol,
            builder: self.builder,
            user_agent: self.user_agent,
            redirect: self.redirect,
            timeout: self.timeout,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: self.body,
        }
    }

    /// Provide a custom transport
    pub fn with_transport<T2>(self, transport: T2) -> Builder<D, T2, P, RP, S, BIn, BOut> {
        Builder {
            resolver: self.resolver,
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
        }
    }

    /// Get a mutable reference to the transport configuration
    pub fn transport(&mut self) -> &mut T {
        &mut self.transport
    }
}

#[cfg(feature = "tls")]
impl<D, T, P, RP, S, BIn, BOut> Builder<D, T, P, RP, S, BIn, BOut> {
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
impl<D, T, P, RP, S, BIn, BOut> Builder<D, T, P, RP, S, BIn, BOut> {
    /// Disable TLS
    pub fn without_tls(self) -> Self {
        self
    }
}

impl<D, T, P, RP, S, BIn, BOut> Builder<D, T, P, RP, S, BIn, BOut> {
    /// Connection pool configuration.
    pub fn pool(&mut self) -> Option<&mut chateau::client::pool::Config> {
        self.pool.as_mut()
    }

    /// Use the provided connection pool configuration.
    pub fn with_pool(mut self, pool: chateau::client::pool::Config) -> Self {
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

impl<D, T, P, RP, S, BIn, BOut> Builder<D, T, P, RP, S, BIn, BOut> {
    /// Use the auto-HTTP Protocol
    pub fn with_auto_http(
        self,
    ) -> Builder<D, T, auto::HttpConnectionBuilder<BIn>, RP, S, BIn, BOut> {
        Builder {
            resolver: self.resolver,
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
        }
    }

    /// Use the provided HTTP connection configuration.
    pub fn with_protocol<P2>(self, protocol: P2) -> Builder<D, T, P2, RP, S, BIn, BOut> {
        Builder {
            resolver: self.resolver,
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
        }
    }

    /// HTTP connection configuration.
    pub fn protocol(&mut self) -> &mut P {
        &mut self.protocol
    }
}

impl<D, T, P, RP, S, BIn, BOut> Builder<D, T, P, RP, S, BIn, BOut> {
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

impl<D, T, P, RP, S, BIn, BOut> Builder<D, T, P, RP, S, BIn, BOut> {
    /// Set the redirect policy. See [`policy`] for more information.
    pub fn with_redirect_policy<RP2>(self, policy: RP2) -> Builder<D, T, P, RP2, S, BIn, BOut> {
        Builder {
            resolver: self.resolver,
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
        }
    }

    /// Disable redirects.
    pub fn without_redirects(self) -> Builder<D, T, P, policy::Standard, S, BIn, BOut> {
        Builder {
            resolver: self.resolver,
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
        }
    }

    /// Set the standard redirect policy. See [`policy::Standard`] for more information.
    pub fn with_standard_redirect_policy(self) -> Builder<D, T, P, policy::Standard, S, BIn, BOut> {
        Builder {
            resolver: self.resolver,
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
        }
    }

    /// Configured redirect policy.
    pub fn redirect_policy(&mut self) -> Option<&mut RP> {
        self.redirect.as_mut()
    }
}

impl<D, T, P, RP, S, BIn, BOut> Builder<D, T, P, RP, S, BIn, BOut> {
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

impl<D, T, P, RP, S, BIn, BOut> Builder<D, T, P, RP, S, BIn, BOut> {
    /// Add a layer to the service under construction
    pub fn with_body<B2In, B2Out>(self) -> Builder<D, T, P, RP, S, B2In, B2Out>
    where
        B2In: Default + Body + Unpin + Send + 'static,
        <B2In as Body>::Data: Send,
        <B2In as Body>::Error: Into<BoxError>,
        B2Out: From<hyper::body::Incoming> + Body + Unpin + Send + 'static,
    {
        Builder {
            resolver: self.resolver,
            transport: self.transport,
            protocol: self.protocol,
            builder: self.builder,
            user_agent: self.user_agent,
            redirect: self.redirect,
            timeout: self.timeout,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
            body: std::marker::PhantomData,
        }
    }
}

impl<D, T, P, RP, S, BIn, BOut> Builder<D, T, P, RP, S, BIn, BOut> {
    /// Add a layer to the service under construction
    pub fn layer<L>(self, layer: L) -> Builder<D, T, P, RP, Stack<L, S>, BIn, BOut> {
        Builder {
            resolver: self.resolver,
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
        }
    }
}

#[cfg(not(feature = "tls"))]
impl<A, D, T, P, RP, S, BIn, BOut> Builder<D, T, P, RP, S, BIn, BOut>
where
    A: Send + 'static,
    D: Resolver<http::Request<BIn>, Address = A> + Clone + Send + Sync + 'static,
    D::Error: Into<BoxError> + std::error::Error + Send + Sync + 'static,
    D::Future: Send + 'static,
    T: Transport<A> + Clone + Send + Sync + 'static,
    T::Error: Into<BoxError>,
    <T as Transport<A>>::IO: PoolableStream + tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    <<T as Transport<A>>::IO as HasConnectionInfo>::Addr: Unpin + Clone + Send,
    P: Protocol<<T as Transport<A>>::IO, http::Request<BIn>> + Clone + Send + Sync + 'static,
    <P as Protocol<<T as Transport<A>>::IO, http::Request<BIn>>>::Error: Into<BoxError>,
    <P as Protocol<<T as Transport<A>>::IO, http::Request<BIn>>>::Connection: Connection<http::Request<BIn>, Response = http::Response<hyper::body::Incoming>>
        + HttpConnection<BIn>
        + PoolableConnection<http::Request<BIn>>,
    <<P as chateau::client::conn::Protocol<<T as Transport<A>>::IO, http::Request<BIn>>>::Connection as Connection<http::Request<BIn>>>::Error: Into<BoxError>,
    RP: policy::Policy<BIn, super::Error> + Clone + Send + Sync + 'static,
    S: tower::Layer<SharedService<http::Request<BIn>, http::Response<BOut>, super::Error>>,
    S::Service: tower::Service<http::Request<BIn>, Response = http::Response<BOut>, Error=super::Error>
        + Clone
        + Send
        + Sync
        + 'static,
    <S::Service as tower::Service<http::Request<BIn>>>::Future: Send + 'static,
    BIn: Default + Body + Unpin + Send + 'static,
    <BIn as Body>::Data: Send,
    <BIn as Body>::Error: Into<BoxError>,
    BOut: From<hyper::body::Incoming> + Body + Unpin + Send + 'static,
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

        let executor = ServiceBuilder::new()
            .layer(SetHostHeaderLayer::new())
            .layer(Http2ChecksLayer::new())
            .layer(Http1ChecksLayer::new())
            .service(ClientExecutorService::new());

        let middleware = self
            .builder
            .layer(SharedService::layer())
            .check_service::<_, _, _, super::Error>()
            .optional(
                self.timeout
                    .map(|d| TimeoutLayer::new(|| super::Error::RequestTimeout, d)),
            )
            .optional(self.redirect.map(FollowRedirectLayer::with_policy))
            .layer(SetRequestHeaderLayer::if_not_present(
                http::header::USER_AGENT,
                user_agent,
            ))
            .layer(IncomingResponseLayer::new());

        let service = SharedService::new(
            middleware
                .map_err(super::Error::from)
                .layer(
                    ConnectionPoolLayer::<_, _, _, http::Request<BIn>, UriKey>::new(
                        self.resolver,
                        self.transport,
                        self.protocol,
                    )
                    .with_optional_pool(self.pool.clone()),
                )
                .service(executor),
        );

        service
    }
}

#[cfg(feature = "tls")]
impl<A, D, T, P, RP, S, BIn, BOut> Builder<D, T, P, RP, S, BIn, BOut>
where
    A: Send + 'static,
    D: Resolver<http::Request<BIn>, Address = A> + Clone + Send + Sync + 'static,
    D::Error: Into<BoxError> + std::error::Error + Send + Sync + 'static,
    D::Future: Send + 'static,
    T: Transport<A> + Clone + Send + Sync + 'static,
    T::Error: Into<BoxError>,
    <T as Transport<A>>::IO: PoolableStream + tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    <<T as Transport<A>>::IO as HasConnectionInfo>::Addr: Unpin + Clone + Send,
    P: Protocol<TlsStream<<T as Transport<A>>::IO>, http::Request<BIn>>
        + Clone
        + Send
        + Sync
        + 'static,
    <P as Protocol<TlsStream<<T as Transport<A>>::IO>, http::Request<BIn>>>::Connection:
        Connection<http::Request<BIn>, Response = http::Response<hyper::body::Incoming>>
            + HttpConnection<BIn>
            + PoolableConnection<http::Request<BIn>>,
    <<P as chateau::client::conn::Protocol<TlsStream<<T as Transport<A>>::IO>, http::Request<BIn>>>::Connection as Connection<http::Request<BIn>>>::Error: Into<BoxError>,

    P: Protocol<<T as Transport<A>>::IO, http::Request<BIn>> + Clone + Send + Sync + 'static,
    <P as Protocol<<T as Transport<A>>::IO, http::Request<BIn>>>::Error: Into<BoxError>,
    <P as Protocol<<T as Transport<A>>::IO, http::Request<BIn>>>::Connection: Connection<http::Request<BIn>, Response = http::Response<hyper::body::Incoming>>
        + HttpConnection<BIn>
        + PoolableConnection<http::Request<BIn>>,
    <<P as chateau::client::conn::Protocol<<T as Transport<A>>::IO, http::Request<BIn>>>::Connection as Connection<http::Request<BIn>>>::Error: Into<BoxError>,
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

        let middleware = self
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
            .layer(IncomingResponseLayer::new());

        let service = if let Some(tls) = self.tls {
            let executor = ServiceBuilder::new()
                .layer(SetHostHeaderLayer::new())
                .layer(Http2ChecksLayer::new())
                .layer(Http1ChecksLayer::new())
                .service(ClientExecutorService::new());
            SharedService::new(
                middleware
                    .check_service::<_, http::Request<BIn>, http::Response<BOut>, super::Error>()
                    .map_err(super::Error::from)
                    .layer(
                        ConnectionPoolLayer::<_, _, _, http::Request<BIn>, UriKey>::new(
                            TlsResolver::new(self.resolver),
                            TlsTransport::new(self.transport, tls.into()),
                            self.protocol,
                        )
                        .with_optional_pool(self.pool.clone()),
                    )
                    .service(executor),
            )
        } else {
            let executor = ServiceBuilder::new()
                .layer(SetHostHeaderLayer::new())
                .layer(Http2ChecksLayer::new())
                .layer(Http1ChecksLayer::new())
                .service(ClientExecutorService::new());
            SharedService::new(
                middleware
                    .map_err(super::Error::from)
                    .check_service::<_, http::Request<BIn>, http::Response<BOut>, super::Error>()
                    .layer(
                        ConnectionPoolLayer::<_, _, _, http::Request<BIn>, UriKey>::new(
                            self.resolver,
                            self.transport,
                            self.protocol,
                        )
                        .with_optional_pool(self.pool.clone()),
                    )
                    .service(executor),
            )
        };

        service
    }
}

#[cfg(not(feature = "tls"))]
impl<A, D, T, P, RP, S> Builder<D, T, P, RP, S, crate::Body, crate::Body>
where
    A: Send + 'static,
    D: Resolver<http::Request<crate::Body>, Address = A> + Clone + Send + Sync + 'static,
    D::Error: Into<BoxError> + std::error::Error + Send + Sync + 'static,
    D::Future: Send + 'static,
    T: Transport<A> + Clone + Send + Sync + 'static,
    T::Error: Into<BoxError>,
    <T as Transport<A>>::IO: PoolableStream + tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    <<T as Transport<A>>::IO as HasConnectionInfo>::Addr: Unpin + Clone + Send,
    P: Protocol<<T as Transport<A>>::IO, http::Request<crate::Body>> + Clone + Send + Sync + 'static,
    <P as Protocol<<T as Transport<A>>::IO, http::Request<crate::Body>>>::Error: Into<BoxError>,
    <P as Protocol<<T as Transport<A>>::IO, http::Request<crate::Body>>>::Connection: Connection<http::Request<crate::Body>, Response = http::Response<hyper::body::Incoming>>
        + HttpConnection<crate::Body>
        + PoolableConnection<http::Request<crate::Body>>,
    <<P as chateau::client::conn::Protocol<<T as Transport<A>>::IO, http::Request<crate::Body>>>::Connection as Connection<http::Request<crate::Body>>>::Error: Into<BoxError>,
    RP: policy::Policy<crate::Body, super::Error> + Clone + Send + Sync + 'static,
    S: tower::Layer<SharedService<http::Request<crate::Body>, http::Response<crate::Body>, super::Error>>,
    S::Service: tower::Service<http::Request<crate::Body>, Response = http::Response<crate::Body>, Error=super::Error>
        + Clone
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

#[cfg(feature = "tls")]
impl<A, D, T, P, RP, S> Builder<D, T, P, RP, S, crate::Body, crate::Body>
where
    A: Send + 'static,
    D: Resolver<http::Request<crate::Body>, Address = A> + Clone + Send + Sync + 'static,
    D::Error: Into<BoxError> + std::error::Error + Send + Sync + 'static,
    D::Future: Send + 'static,
    T: Transport<A> + Clone + Send + Sync + 'static,
    T::Error: Into<BoxError>,
    <T as Transport<A>>::IO: PoolableStream + tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    <<T as Transport<A>>::IO as HasConnectionInfo>::Addr: Unpin + Clone + Send,
    P: Protocol<<T as Transport<A>>::IO, http::Request<crate::Body>>
        + Clone
        + Send
        + Sync
        + 'static,
    <P as Protocol<<T as Transport<A>>::IO, http::Request<crate::Body>>>::Error: Into<BoxError>,
    <P as Protocol<<T as Transport<A>>::IO, http::Request<crate::Body>>>::Connection:
        Connection<
                http::Request<crate::Body>,
                Response = http::Response<hyper::body::Incoming>,
                Error = super::Error,
            > + HttpConnection<crate::Body>
            + PoolableConnection<http::Request<crate::Body>>,
    P: Protocol<TlsStream<<T as Transport<A>>::IO>, http::Request<crate::Body>>
        + Clone
        + Send
        + Sync
        + 'static,
    <P as Protocol<TlsStream<<T as Transport<A>>::IO>, http::Request<crate::Body>>>::Connection:
        Connection<
                http::Request<crate::Body>,
                Response = http::Response<hyper::body::Incoming>,
                Error = super::Error,
            > + HttpConnection<crate::Body>
            + PoolableConnection<http::Request<crate::Body>>,
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
