use http::uri::Port;
use http::uri::Scheme;
use http::HeaderValue;
use http::Uri;
use http::Version;
use hyper::body::Incoming;
use thiserror::Error;
use tower::ServiceExt;

mod builder;
mod conn;
mod lazy;
mod pool;

use self::conn::HttpConnector;
use self::pool::{Poolable, Pooled};

pub use conn::Connect;
pub use conn::ConnectionError;
pub use conn::ConnectionProtocol;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Connection(ConnectionError),

    #[error("user error: {0}")]
    User(hyper::Error),

    #[error("invalid method: {0}")]
    InvalidMethod(http::Method),

    #[error("unsupported protocol")]
    UnsupportedProtocol,
}

impl From<pool::Error<ConnectionError>> for Error {
    fn from(error: pool::Error<ConnectionError>) -> Self {
        match error {
            pool::Error::Connecting(error) => Error::Connection(error),
        }
    }
}

impl From<hyper::Error> for Error {
    fn from(error: hyper::Error) -> Self {
        if error.is_user() {
            Error::User(error)
        } else if error.is_closed() {
            Error::Connection(ConnectionError::Closed(error))
        } else if error.is_canceled() {
            Error::Connection(ConnectionError::Canceled(error))
        } else if error.is_timeout() {
            Error::Connection(ConnectionError::Timeout)
        } else {
            panic!("unknown error type: {:?}", error);
        }
    }
}

pub fn default_tls_config() -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().expect("could not load platform certs") {
        roots.add(cert).unwrap();
    }

    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

#[derive(Debug)]
pub struct Client<C> {
    connector: C,
    pool: pool::Pool<conn::ClientConnection>,
    protocol: ConnectionProtocol,
}

impl<C> Clone for Client<C>
where
    C: Clone,
{
    fn clone(&self) -> Self {
        Self {
            connector: self.connector.clone(),
            pool: self.pool.clone(),
            protocol: self.protocol,
        }
    }
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
            protocol: ConnectionProtocol::Http1,
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
            .checkout(key, self.protocol.multiplex(), move || async move {
                let mut conn = connecting.await?;
                conn.when_ready()
                    .await
                    .map_err(ConnectionError::Handshake)?;
                Ok(conn)
            })
            .await
    }

    pub async fn request(
        &self,
        mut request: arnold::Request,
    ) -> Result<http::Response<Incoming>, Error> {
        let uri = request.uri().clone();

        let mut conn = self.connect_to(uri).await?;

        request
            .headers_mut()
            .entry(http::header::USER_AGENT)
            .or_insert_with(|| {
                HeaderValue::from_static(concat!(
                    env!("CARGO_PKG_NAME"),
                    "/",
                    env!("CARGO_PKG_VERSION")
                ))
            });

        if conn.version() == Version::HTTP_11 {
            if request.version() == Version::HTTP_2 {
                return Err(Error::UnsupportedProtocol);
            }

            //TODO: Configure set host header
            let uri = request.uri().clone();
            request
                .headers_mut()
                .entry(http::header::HOST)
                .or_insert_with(|| {
                    let hostname = uri.host().expect("authority implies host");
                    if let Some(port) = get_non_default_port(&uri) {
                        let s = format!("{}:{}", hostname, port);
                        HeaderValue::from_str(&s)
                    } else {
                        HeaderValue::from_str(hostname)
                    }
                    .expect("uri host is valid header value")
                });

            if request.method() == http::Method::CONNECT {
                authority_form(request.uri_mut());
            } else if request.uri().scheme().is_none() || request.uri().authority().is_none() {
                absolute_form(request.uri_mut());
            } else {
                origin_form(request.uri_mut());
            }
        } else if request.method() == http::Method::CONNECT {
            return Err(Error::InvalidMethod(http::Method::CONNECT));
        } else {
            absolute_form(request.uri_mut());
        }

        let response = conn.send_request(request).await?;

        // Shared connections are already in the pool, no need to do this.
        if !conn.can_share() {
            // Only re-insert the connection when it is ready again. Spawn
            // a task to wait for the connection to become ready before dropping.
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
    pub async fn get(&mut self, uri: http::Uri) -> Result<http::Response<Incoming>, Error> {
        let request = http::Request::get(uri.clone())
            .body(arnold::Body::empty())
            .unwrap();

        let response = self.request(request).await?;
        Ok(response)
    }
}

fn authority_form(uri: &mut Uri) {
    *uri = match uri.authority() {
        Some(auth) => {
            let mut parts = ::http::uri::Parts::default();
            parts.authority = Some(auth.clone());
            Uri::from_parts(parts).expect("authority is valid")
        }
        None => {
            unreachable!("authority_form with relative uri");
        }
    };
}

fn absolute_form(uri: &mut Uri) {
    debug_assert!(uri.scheme().is_some(), "absolute_form needs a scheme");
    debug_assert!(
        uri.authority().is_some(),
        "absolute_form needs an authority"
    );
    // If the URI is to HTTPS, and the connector claimed to be a proxy,
    // then it *should* have tunneled, and so we don't want to send
    // absolute-form in that case.
    if uri.scheme() == Some(&Scheme::HTTPS) {
        origin_form(uri);
    }
}

fn origin_form(uri: &mut Uri) {
    let path = match uri.path_and_query() {
        Some(path) if path.as_str() != "/" => {
            let mut parts = ::http::uri::Parts::default();
            parts.path_and_query = Some(path.clone());
            Uri::from_parts(parts).expect("path is valid uri")
        }
        _none_or_just_slash => {
            debug_assert!(Uri::default() == "/");
            Uri::default()
        }
    };
    *uri = path
}

fn get_non_default_port(uri: &Uri) -> Option<Port<&str>> {
    match (uri.port().map(|p| p.as_u16()), is_schema_secure(uri)) {
        (Some(443), true) => None,
        (Some(80), false) => None,
        _ => uri.port(),
    }
}

fn is_schema_secure(uri: &Uri) -> bool {
    uri.scheme_str()
        .map(|scheme_str| matches!(scheme_str, "wss" | "https"))
        .unwrap_or_default()
}
