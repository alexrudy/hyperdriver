use rustls::ClientConfig;

use crate::{
    conn::{http::HttpConnectionBuilder, HttpConnector},
    default_tls_config, Client,
};

#[derive(Debug)]
pub struct Builder {
    tcp: crate::conn::TcpConnectionConfig,
    tls: Option<ClientConfig>,
    pool: Option<crate::pool::Config>,
    conn: crate::conn::http::HttpConnectionBuilder,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            tcp: Default::default(),
            tls: Some(default_tls_config()),
            pool: Some(Default::default()),
            conn: Default::default(),
        }
    }
}

impl Builder {
    pub fn tcp(&mut self) -> &mut crate::conn::TcpConnectionConfig {
        &mut self.tcp
    }

    pub fn with_tls(&mut self, config: ClientConfig) -> &mut Self {
        self.tls = Some(config);
        self
    }

    pub fn pool(&mut self) -> &mut Option<crate::pool::Config> {
        &mut self.pool
    }

    pub fn conn(&mut self) -> &mut crate::conn::http::HttpConnectionBuilder {
        &mut self.conn
    }
}

impl Builder {
    pub fn build(self) -> Client<HttpConnector> {
        let tls = self.tls.unwrap_or_else(super::default_tls_config);

        Client {
            transport: crate::conn::TcpConnector::new(self.tcp, tls),
            protocol: HttpConnector::new(HttpConnectionBuilder::default()),
            pool: self.pool.map(crate::pool::Pool::new),
        }
    }
}
