//! # Patron
//!
//! Patron is a HTTP client library for Rust, built on top of [hyper] and [braid].

#![warn(missing_docs)]
#![warn(missing_debug_implementations)]
#![deny(unsafe_code)]

use std::fmt;
use std::future::Future;
use std::task::Poll;

use conn::Connection;
use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use http::uri::Port;
use http::uri::Scheme;
use http::HeaderValue;
use http::Uri;
use http::Version;
use hyper::body::Incoming;
use pool::Checkout;
use thiserror::Error;
use tower::ServiceExt;
use tracing::instrument::Instrumented;
use tracing::warn;

mod builder;
mod conn;
mod lazy;
mod pool;

pub use self::conn::http::HttpConnector;
use self::pool::{Poolable, Pooled};

pub use conn::Connect;
pub use conn::ConnectionError;
pub use conn::ConnectionProtocol;

/// Client error type.
#[derive(Debug, Error)]
pub enum Error {
    /// Error occured with the underlying connection.
    #[error(transparent)]
    Connection(Box<dyn std::error::Error + Send + Sync + 'static>),

    /// Error occured with the underlying transport.
    #[error("transport: {0}")]
    Transport(Box<dyn std::error::Error + Send + Sync + 'static>),

    /// Error occured with the user's request, such as an invalid URI.
    #[error("user error: {0}")]
    User(hyper::Error),

    /// Invalid HTTP Method for the current action.
    #[error("invalid method: {0}")]
    InvalidMethod(http::Method),

    /// Protocol is not supported by this client or transport.
    #[error("unsupported protocol")]
    UnsupportedProtocol,
}

impl From<pool::Error<ConnectionError>> for Error {
    fn from(error: pool::Error<ConnectionError>) -> Self {
        match error {
            pool::Error::Connecting(error) => Error::Connection(error.into()),
        }
    }
}

/// Get a default TLS client configuration by loading the platform's native certificates.
pub fn default_tls_config() -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().expect("could not load platform certs") {
        roots.add(cert).unwrap();
    }

    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// An HTTP client
#[derive(Debug)]
pub struct Client<C = HttpConnector>
where
    C: Connect,
{
    connector: C,
    pool: pool::Pool<C::Connection>,
}

impl<C> Client<C>
where
    C: Connect,
{
    /// Create a new client with the given connector and pool configuration.
    pub fn new(connector: C, pool: pool::Config) -> Self {
        Self {
            connector,
            pool: pool::Pool::new(pool),
        }
    }
}

impl<C> Clone for Client<C>
where
    C: Connect + Clone,
{
    fn clone(&self) -> Self {
        Self {
            connector: self.connector.clone(),
            pool: self.pool.clone(),
        }
    }
}

impl Client<HttpConnector> {
    /// A client builder for configuring the client.
    pub fn builder() -> builder::Builder {
        builder::Builder::default()
    }

    /// Create a new client with the default configuration.
    pub fn new_tcp_http() -> Self {
        Self {
            pool: pool::Pool::new(pool::Config {
                idle_timeout: Some(std::time::Duration::from_secs(90)),
                max_idle_per_host: 32,
            }),
            connector: conn::HttpConnector::new(
                conn::TcpConnector::new(conn::TcpConnectionConfig::default(), default_tls_config()),
                conn::http::HttpConnectionBuilder::default(),
            ),
        }
    }
}

impl Default for Client<HttpConnector> {
    fn default() -> Self {
        Self::new_tcp_http()
    }
}

impl<C, T> Client<C>
where
    C: Connect<Connection = T> + Clone + Send + 'static,
    C: tower::Service<Uri, Response = T>,
    <C as tower::Service<Uri>>::Error: std::error::Error + Send + Sync + 'static,
    <C as tower::Service<Uri>>::Future: Send + 'static,
    T: Connection + Poolable,
{
    fn connect_to(
        &self,
        uri: http::Uri,
        protocol: &ConnectionProtocol,
    ) -> Instrumented<Checkout<C::Connection, ConnectionError>> {
        let key: pool::Key = uri.clone().into();

        let connecting = self.connector.clone().oneshot(uri);

        //TODO: How do we handle potential multiplexing here? Really, the connector should decide?
        self.pool
            .checkout(key, protocol.multiplex(), move || async move {
                let mut conn = connecting
                    .await
                    .map_err(|error| ConnectionError::Connecting(error.into()))?;
                conn.when_ready()
                    .await
                    .map_err(|error| ConnectionError::Handshake(error.into()))?;
                Ok(conn)
            })
    }

    /// Send an http Request, and return a Future of the Response.
    pub fn request(&self, request: arnold::Request) -> ResponseFuture<C::Connection> {
        let uri = request.uri().clone();

        let protocol: ConnectionProtocol = request.version().into();

        let checkout = self.connect_to(uri, &protocol);
        ResponseFuture::new(checkout, request)
    }
}

impl<C, T> Client<C>
where
    C: Connect<Connection = T> + Clone + Send + 'static,
    C: tower::Service<Uri, Response = T>,
    <C as tower::Service<Uri>>::Error: std::error::Error + Send + Sync + 'static,
    <C as tower::Service<Uri>>::Future: Send + 'static,
    T: Connection + Poolable,
{
    /// Make a GET request to the given URI.
    pub async fn get(&mut self, uri: http::Uri) -> Result<http::Response<Incoming>, Error> {
        let request = http::Request::get(uri.clone())
            .body(arnold::Body::empty())
            .unwrap();

        let response = self.request(request).await?;
        Ok(response)
    }
}

impl<C, T> tower::Service<http::Request<arnold::Body>> for Client<C>
where
    C: Connect<Connection = T> + Clone + Send + 'static,
    C: tower::Service<Uri, Response = T>,
    <C as tower::Service<Uri>>::Error: std::error::Error + Send + Sync + 'static,
    <C as tower::Service<Uri>>::Future: Send + 'static,
    T: Connection + Poolable,
{
    type Response = http::Response<Incoming>;
    type Error = Error;
    type Future = ResponseFuture<C::Connection>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<arnold::Body>) -> Self::Future {
        self.request(req)
    }
}

/// A future that resolves to an HTTP response.
pub struct ResponseFuture<C: pool::Poolable> {
    inner: ResponseFutureState<C>,
}

impl<C: pool::Poolable> fmt::Debug for ResponseFuture<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponseFuture").finish()
    }
}

impl<C: pool::Poolable> ResponseFuture<C> {
    fn new(checkout: Instrumented<Checkout<C, ConnectionError>>, request: arnold::Request) -> Self {
        Self {
            inner: ResponseFutureState::Checkout { checkout, request },
        }
    }
}

impl<C: Connection + pool::Poolable> Future for ResponseFuture<C> {
    type Output = Result<http::Response<Incoming>, Error>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        loop {
            match std::mem::replace(&mut self.inner, ResponseFutureState::Empty) {
                ResponseFutureState::Checkout {
                    mut checkout,
                    request,
                } => match checkout.poll_unpin(cx) {
                    Poll::Ready(Ok(conn)) => {
                        self.inner =
                            ResponseFutureState::Request(execute_request(request, conn).boxed());
                    }
                    Poll::Ready(Err(error)) => {
                        return Poll::Ready(Err(error.into()));
                    }
                    Poll::Pending => {
                        self.inner = ResponseFutureState::Checkout { checkout, request };
                        return Poll::Pending;
                    }
                },
                ResponseFutureState::Request(mut fut) => match fut.poll_unpin(cx) {
                    Poll::Ready(outcome) => {
                        return Poll::Ready(outcome);
                    }
                    Poll::Pending => {
                        self.inner = ResponseFutureState::Request(fut);
                        return Poll::Pending;
                    }
                },
                ResponseFutureState::Empty => {
                    panic!("future polled after completion");
                }
            }
        }
    }
}

enum ResponseFutureState<C: pool::Poolable> {
    Empty,
    Checkout {
        checkout: Instrumented<Checkout<C, ConnectionError>>,
        request: arnold::Request,
    },
    Request(BoxFuture<'static, Result<http::Response<Incoming>, Error>>),
}

async fn execute_request<C: Connection + Poolable>(
    mut request: arnold::Request,
    mut conn: Pooled<C>,
) -> Result<http::Response<Incoming>, Error> {
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
            warn!("refusing to send HTTP/2 request to HTTP/1.1 connection");
            return Err(Error::UnsupportedProtocol);
        }

        //TODO: Configure set host header?
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

    let response = conn
        .send_request(request)
        .await
        .map_err(|error| Error::Connection(error.into()))?;

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
