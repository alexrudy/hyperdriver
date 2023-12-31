use rustls::ClientConfig;

use crate::{conn::HttpConnector, Client};

#[derive(Debug, Default)]
pub struct Builder {
    tcp: crate::conn::TcpConnectionConfig,
    tls: Option<ClientConfig>,
    pool: crate::pool::Config,
    conn: crate::conn::Builder,
}

impl Builder {
    pub fn tcp(&mut self) -> &mut crate::conn::TcpConnectionConfig {
        &mut self.tcp
    }

    pub fn with_tls(&mut self, config: ClientConfig) -> &mut Self {
        self.tls = Some(config);
        self
    }

    pub fn pool(&mut self) -> &mut crate::pool::Config {
        &mut self.pool
    }

    pub fn conn(&mut self) -> &mut crate::conn::Builder {
        &mut self.conn
    }
}

impl Builder {
    pub fn build(self) -> Client<HttpConnector> {
        let tls = self.tls.unwrap_or_else(super::default_tls_config);

        Client {
            connector: HttpConnector::new(crate::conn::TcpConnector::new(self.tcp, tls), self.conn),
            pool: crate::pool::Pool::new(self.pool),
        }
    }
}
