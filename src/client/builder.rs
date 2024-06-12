use tokio::net::TcpStream;

#[cfg(feature = "tls")]
use rustls::ClientConfig;

use crate::client::{conn::http::HttpConnectionBuilder, Client};

#[cfg(feature = "tls")]
use crate::client::default_tls_config;

use super::conn::{dns::GaiResolver, TcpConnector};

/// A builder for a client.
#[derive(Debug)]
pub struct Builder {
    tcp: crate::client::conn::TcpConnectionConfig,
    #[cfg(feature = "tls")]
    tls: Option<ClientConfig>,
    pool: Option<crate::client::pool::Config>,
    conn: crate::client::conn::http::HttpConnectionBuilder,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            tcp: Default::default(),
            #[cfg(feature = "tls")]
            tls: Some(default_tls_config()),
            pool: Some(Default::default()),
            conn: Default::default(),
        }
    }
}

impl Builder {
    /// Use the provided TCP configuration.
    pub fn with_tcp(mut self, config: crate::client::conn::TcpConnectionConfig) -> Self {
        self.tcp = config;
        self
    }

    /// TCP configuration.
    pub fn tcp(&mut self) -> &mut crate::client::conn::TcpConnectionConfig {
        &mut self.tcp
    }
}

#[cfg(feature = "tls")]
impl Builder {
    /// Use the provided TLS configuration.
    pub fn with_tls(&mut self, config: ClientConfig) -> &mut Self {
        self.tls = Some(config);
        self
    }

    /// Use the default TLS configuration with native root certificates.
    pub fn with_default_tls(&mut self) -> &mut Self {
        self.tls = Some(default_tls_config());
        self
    }
}

impl Builder {
    /// Connection pool configuration.
    pub fn pool(&mut self) -> &mut Option<crate::client::pool::Config> {
        &mut self.pool
    }

    /// Use the provided connection pool configuration.
    pub fn with_pool(mut self, pool: crate::client::pool::Config) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Disable connection pooling.
    pub fn without_pool(mut self) -> Self {
        self.pool = None;
        self
    }

    /// HTTP connection configuration.
    pub fn conn(&mut self) -> &mut crate::client::conn::http::HttpConnectionBuilder {
        &mut self.conn
    }
}

impl Builder {
    /// Build the client.
    pub fn build(self) -> Client<HttpConnectionBuilder, TcpConnector<GaiResolver, TcpStream>> {
        Client {
            #[cfg(feature = "tls")]
            transport: crate::client::conn::TcpConnector::builder()
                .with_config(self.tcp)
                .with_optional_tls(self.tls)
                .build_with_gai(),

            #[cfg(not(feature = "tls"))]
            transport: crate::client::conn::TcpConnector::builder()
                .with_config(self.tcp)
                .build_with_gai(),

            protocol: HttpConnectionBuilder::default(),
            pool: self.pool.map(crate::client::pool::Pool::new),

            _body: std::marker::PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder() {
        let client = Builder::default().build();
        assert!(client.pool.is_some());
    }

    #[test]
    fn test_builder_tcp() {
        let mut builder = Builder::default();
        builder.tcp().nodelay = true;

        let client = builder.build();
        assert!(client.transport.config().nodelay)
    }

    #[cfg(feature = "tls")]
    #[test]
    fn test_builder_tls() {
        let mut builder = Builder::default();
        let mut tls = super::default_tls_config();
        tls.alpn_protocols.push(b"a1".to_vec());
        builder.with_tls(tls);

        let client = builder.build();
        assert_eq!(
            client.transport.tls().unwrap().alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec(), b"a1".to_vec()]
        );
    }
}
