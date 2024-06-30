#[cfg(feature = "tls")]
use std::sync::Arc;

use http::HeaderValue;
#[cfg(feature = "tls")]
use rustls::ClientConfig;
use tower::ServiceBuilder;
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
use crate::service::SharedService;

pub trait BuildProtocol<IO>
where
    IO: HasConnectionInfo,
{
    type Target: Protocol<IO>;
    fn build(self) -> Self::Target;
}

impl<P, IO> BuildProtocol<IO> for P
where
    P: Protocol<IO>,
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
pub struct Builder<T, P> {
    transport: T,
    protocol: P,
    user_agent: Option<String>,
    #[cfg(feature = "tls")]
    tls: Option<ClientConfig>,
    pool: Option<crate::client::pool::Config>,
}

impl Builder<(), ()> {
    /// Create a new, empty builder
    pub fn new() -> Self {
        Self {
            transport: (),
            protocol: (),
            user_agent: None,
            #[cfg(feature = "tls")]
            tls: None,
            pool: None,
        }
    }
}

impl Default for Builder<TcpTransportConfig, HttpConnectionBuilder> {
    fn default() -> Self {
        Self {
            transport: Default::default(),
            protocol: Default::default(),
            user_agent: None,
            #[cfg(feature = "tls")]
            tls: Some(default_tls_config()),
            pool: Some(Default::default()),
        }
    }
}

impl<T, P> Builder<T, P> {
    /// Use the provided TCP configuration.
    pub fn with_tcp(self, config: TcpTransportConfig) -> Builder<TcpTransportConfig, P> {
        Builder {
            transport: config,
            protocol: self.protocol,
            user_agent: self.user_agent,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
        }
    }

    /// Provide a custom transport
    pub fn with_transport<T2>(self, transport: T2) -> Builder<T2, P> {
        Builder {
            transport,
            protocol: self.protocol,
            user_agent: self.user_agent,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
        }
    }
}

#[cfg(feature = "tls")]
impl<T, P> Builder<T, P> {
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
impl<T, P> Builder<T, P> {
    /// Disable TLS
    pub fn without_tls(self) -> Self {
        self
    }
}

impl<T, P> Builder<T, P> {
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

impl<T, P> Builder<T, P> {
    /// Use the auto-HTTP Protocol
    pub fn with_auto_http(self) -> Builder<T, auto::HttpConnectionBuilder> {
        Builder {
            transport: self.transport,
            protocol: auto::HttpConnectionBuilder::default(),
            user_agent: self.user_agent,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
        }
    }

    /// Use the provided HTTP connection configuration.
    pub fn with_protocol<P2>(self, protocol: P2) -> Builder<T, P2> {
        Builder {
            transport: self.transport,
            protocol,
            user_agent: self.user_agent,
            #[cfg(feature = "tls")]
            tls: self.tls,
            pool: self.pool,
        }
    }

    /// HTTP connection configuration.
    pub fn protocol(&mut self) -> &mut P {
        &mut self.protocol
    }
}

impl<T, P> Builder<T, P>
where
    T: BuildTransport,
    <T as BuildTransport>::Target: Transport + Clone + Send + Sync + 'static,
    <<T as BuildTransport>::Target as Transport>::IO:
        tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    <<<T as BuildTransport>::Target as Transport>::IO as HasConnectionInfo>::Addr:
        Unpin + Clone + Send,
    P: BuildProtocol<super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>>,
    <P as BuildProtocol<
        super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
    >>::Target: Protocol<
            super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
            Error = ConnectionError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <<P as BuildProtocol<
        super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
    >>::Target as Protocol<
        super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
    >>::Connection: PoolableConnection,
    crate::Body: From<
        <<<P as BuildProtocol<
            super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
        >>::Target as Protocol<
            super::conn::stream::Stream<<<T as BuildTransport>::Target as Transport>::IO>,
        >>::Connection as Connection>::ResBody,
    >,
{
    /// Build the client.
    pub fn build(self) -> Client {
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

        let service = ServiceBuilder::new()
            .layer(SharedService::layer())
            .layer(SetRequestHeaderLayer::if_not_present(
                http::header::USER_AGENT,
                user_agent,
            ))
            .layer(FollowRedirectLayer::new())
            .service(ClientService {
                transport,
                protocol: self.protocol.build(),
                pool: self.pool.map(super::pool::Pool::new),
                _body: std::marker::PhantomData,
            });

        Client::new_from_service(service)
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
