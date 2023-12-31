use conn::{Connect, HttpConnector};
use hyper::body::{Body as _, Incoming};
use pool::{Poolable, Pooled};
use tower::ServiceExt;

mod builder;
mod conn;
mod lazy;
mod pool;

pub use conn::ConnectionError;
pub use conn::ConnectionProtocol;

pub fn default_tls_config() -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().expect("could not load platform certs") {
        roots.add(cert).unwrap();
    }

    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

pub struct Client<C> {
    connector: C,
    pool: pool::Pool<conn::ClientConnection>,
}

impl Client<HttpConnector> {
    pub fn builder() -> builder::Builder {
        builder::Builder::default()
    }

    pub fn new() -> Self {
        Self {
            pool: pool::Pool::new(pool::Config {
                idle_timeout: Some(std::time::Duration::from_secs(90)),
                max_idle_per_host: 32,
            }),
            connector: conn::HttpConnector::new(
                conn::TcpConnector::new(conn::TcpConnectionConfig::default(), default_tls_config()),
                conn::Builder::default(),
            ),
        }
    }
}

impl Default for Client<HttpConnector> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> Client<C>
where
    C: Connect + Clone,
{
    async fn connect_to(
        &self,
        uri: http::Uri,
    ) -> Result<Pooled<conn::ClientConnection>, pool::Error<conn::ConnectionError>> {
        let key: pool::Key = uri.clone().into();

        let connecting = self.connector.clone().oneshot(uri);

        //TODO: How do we handle potential multiplexing here? Really, the connector should decide?
        self.pool
            .checkout(key, false, move || async move {
                let mut conn = connecting.await?;
                conn.when_ready()
                    .await
                    .map_err(ConnectionError::Handshake)?;
                Ok(conn)
            })
            .await
    }

    pub async fn send_request(
        &self,
        request: arnold::Request,
    ) -> Result<http::Response<Incoming>, hyper::Error> {
        let uri = request.uri().clone();

        let mut conn = self.connect_to(uri).await.expect("connection failed");
        let response = conn.send_request(request).await?;

        if !conn.can_share() && !response.body().is_end_stream() {
            tokio::spawn(async move {
                let _ = conn.when_ready().await.map_err(|_| ());
            });
        }

        Ok(response)
    }
}

impl<C> Client<C>
where
    C: Connect + Clone,
{
    pub async fn get(&mut self, uri: http::Uri) -> Result<http::Response<Incoming>, hyper::Error> {
        let request = http::Request::get(uri.clone())
            .body(arnold::Body::empty())
            .unwrap();

        let response = self.send_request(request).await?;
        Ok(response)
    }
}
