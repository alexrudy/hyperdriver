use std::fmt;
use std::future::poll_fn;
use std::future::Future;
use std::task::Poll;

use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use http::uri::Port;
use http::uri::Scheme;
use http::HeaderValue;
use http::Uri;
use http::Version;
use hyper::body::Incoming;
use tracing::warn;

#[cfg(feature = "stream")]
use crate::client::conn::tcp::TcpConnectionConfig;
#[cfg(feature = "stream")]
use crate::client::conn::tcp::TcpConnector;
use crate::client::conn::Connection;
use crate::client::conn::ConnectionError;
use crate::client::conn::HttpProtocol;
use crate::client::conn::Protocol;
use crate::client::conn::Transport;
use crate::client::conn::TransportStream;
use crate::client::pool::Checkout;
use crate::client::pool::Connector;
use crate::client::pool::{self, PoolableConnection, Pooled};
use crate::client::Error;

#[cfg(feature = "stream")]
use crate::client::HttpConnectionBuilder;
use crate::stream::info::HasConnectionInfo;

#[cfg(feature = "tls")]
use crate::client::default_tls_config;

#[cfg(feature = "stream")]
mod builder;

#[cfg(feature = "stream")]
/// An HTTP client
#[derive(Debug)]
pub struct Client<P = HttpConnectionBuilder, T = TcpConnector>
where
    T: Transport,
    P: Protocol<T::IO>,
    P::Connection: PoolableConnection,
{
    protocol: P,
    transport: T,
    pool: Option<pool::Pool<P::Connection>>,
}

#[cfg(not(feature = "stream"))]
/// An HTTP client
#[derive(Debug)]
pub struct Client<P, T>
where
    T: Transport,
    P: Protocol<T::IO>,
    P::Connection: PoolableConnection,
{
    protocol: P,
    transport: T,
    pool: Option<pool::Pool<P::Connection>>,
}

impl<P, T> Client<P, T>
where
    T: Transport,
    P: Protocol<T::IO>,
    P::Connection: PoolableConnection,
{
    /// Create a new client with the given connector and pool configuration.
    pub fn new(connector: P, transport: T, pool: pool::Config) -> Self {
        Self {
            protocol: connector,
            transport,
            pool: Some(pool::Pool::new(pool)),
        }
    }
}

impl<P, T> Clone for Client<P, T>
where
    P: Protocol<T::IO> + Clone,
    P::Connection: PoolableConnection,
    T: Transport + Clone,
{
    fn clone(&self) -> Self {
        Self {
            protocol: self.protocol.clone(),
            transport: self.transport.clone(),
            pool: self.pool.clone(),
        }
    }
}

#[cfg(feature = "stream")]
impl Client<HttpConnectionBuilder, TcpConnector> {
    /// A client builder for configuring the client.
    pub fn builder() -> builder::Builder {
        builder::Builder::default()
    }

    /// Create a new client with the default configuration.
    pub fn new_tcp_http() -> Self {
        Self {
            pool: Some(pool::Pool::new(pool::Config {
                idle_timeout: Some(std::time::Duration::from_secs(90)),
                max_idle_per_host: 32,
            })),

            #[cfg(feature = "tls")]
            transport: TcpConnector::new(TcpConnectionConfig::default(), default_tls_config()),

            #[cfg(not(feature = "tls"))]
            transport: TcpConnector::new(TcpConnectionConfig::default()),

            protocol: HttpConnectionBuilder::default(),
        }
    }
}

#[cfg(feature = "stream")]
impl Default for Client<HttpConnectionBuilder> {
    fn default() -> Self {
        Self::new_tcp_http()
    }
}

impl<P, C, T> Client<P, T>
where
    C: Connection + PoolableConnection,
    P: Protocol<T::IO, Connection = C, Error = ConnectionError> + Clone + Send + Sync + 'static,
    T: Transport + 'static,
    T::IO: Unpin,
    <<T as Transport>::IO as HasConnectionInfo>::Addr: Send,
{
    fn connect_to(
        &self,
        uri: http::Uri,
        http_protocol: HttpProtocol,
    ) -> Checkout<P::Connection, TransportStream<T::IO>, ConnectionError> {
        let key: pool::Key = uri.clone().into();

        let mut protocol = self.protocol.clone();
        let mut transport = self.transport.clone();

        let connector = Connector::new(
            move || async move {
                poll_fn(|cx| Transport::poll_ready(&mut transport, cx))
                    .await
                    .map_err(|error| ConnectionError::Connecting(error.into()))?;
                transport
                    .connect(uri)
                    .await
                    .map_err(|error| ConnectionError::Connecting(error.into()))
            },
            Box::new(move |transport| {
                Box::pin(async move {
                    poll_fn(|cx| Protocol::poll_ready(&mut protocol, cx))
                        .await
                        .map_err(|error| ConnectionError::Handshake(error.into()))?;
                    protocol
                        .connect(transport, http_protocol)
                        .await
                        .map_err(|error| ConnectionError::Handshake(error.into()))
                }) as _
            }),
        );

        if let Some(pool) = self.pool.as_ref() {
            pool.checkout(key, http_protocol.multiplex(), connector)
        } else {
            Checkout::detached(key, connector)
        }
    }

    /// Send an http Request, and return a Future of the Response.
    pub fn request(
        &self,
        request: crate::body::Request,
    ) -> ResponseFuture<P::Connection, TransportStream<T::IO>> {
        let uri = request.uri().clone();

        let protocol: HttpProtocol = request.version().into();

        let checkout = self.connect_to(uri, protocol);
        ResponseFuture::new(checkout, request)
    }

    /// Make a GET request to the given URI.
    pub async fn get(&mut self, uri: http::Uri) -> Result<http::Response<Incoming>, Error> {
        let request = http::Request::get(uri.clone())
            .body(crate::body::Body::empty())
            .unwrap();

        let response = self.request(request).await?;
        Ok(response)
    }
}

impl<P, C, T> tower::Service<http::Request<crate::body::Body>> for Client<P, T>
where
    C: Connection + PoolableConnection,
    P: Protocol<T::IO, Connection = C, Error = ConnectionError> + Clone + Send + Sync + 'static,
    T: Transport + 'static,
    T::IO: Unpin,
    <<T as Transport>::IO as HasConnectionInfo>::Addr: Send,
{
    type Response = http::Response<Incoming>;
    type Error = Error;
    type Future = ResponseFuture<P::Connection, TransportStream<T::IO>>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<crate::body::Body>) -> Self::Future {
        self.request(req)
    }
}

/// A future that resolves to an HTTP response.
pub struct ResponseFuture<C, T>
where
    C: pool::PoolableConnection,
    T: pool::PoolableTransport,
{
    inner: ResponseFutureState<C, T>,
}

impl<C: pool::PoolableConnection, T: pool::PoolableTransport> fmt::Debug for ResponseFuture<C, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponseFuture").finish()
    }
}

impl<C, T> ResponseFuture<C, T>
where
    C: pool::PoolableConnection,
    T: pool::PoolableTransport,
{
    fn new(checkout: Checkout<C, T, ConnectionError>, request: crate::body::Request) -> Self {
        Self {
            inner: ResponseFutureState::Checkout { checkout, request },
        }
    }
}

impl<C, T> Future for ResponseFuture<C, T>
where
    C: Connection + pool::PoolableConnection,
    T: pool::PoolableTransport,
{
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

enum ResponseFutureState<C: pool::PoolableConnection, T: pool::PoolableTransport> {
    Empty,
    Checkout {
        checkout: Checkout<C, T, ConnectionError>,
        request: crate::body::Request,
    },
    Request(BoxFuture<'static, Result<http::Response<Incoming>, Error>>),
}

async fn execute_request<C: Connection + PoolableConnection>(
    mut request: crate::body::Request,
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
        set_host_header(&mut request);

        if request.method() == http::Method::CONNECT {
            authority_form(request.uri_mut());
        } else if request.uri().scheme().is_none() || request.uri().authority().is_none() {
            absolute_form(request.uri_mut());
        } else {
            origin_form(request.uri_mut());
        }
    } else if request.method() == http::Method::CONNECT {
        return Err(Error::InvalidMethod(http::Method::CONNECT));
    } else if conn.version() == Version::HTTP_2 {
        set_host_header(&mut request);
    }

    tracing::trace!(request.uri=%request.uri(), conn.version=?conn.version(), req.version=?request.version(), "sending request");

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

fn set_host_header<B>(request: &mut http::Request<B>) {
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
}
