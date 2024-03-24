use ::http::Uri;
use braid::client::Stream;
use braid::info::ConnectionInfo;
use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use std::future::Future;
use std::io;

use hyper::body::Incoming;
use tower::Service;

pub mod dns;
pub mod duplex;
pub mod http;
pub mod tcp;

use crate::pool::PoolableConnection;
use crate::pool::PoolableTransport;

pub use self::http::ConnectionError;
pub(crate) use self::http::HttpConnector;
pub(crate) use self::tcp::TcpConnectionConfig;
pub(crate) use self::tcp::TcpConnector;

/// A transport provides data transmission between two endpoints.
pub trait Transport: Clone + Send
where
    Self: Service<Uri, Response = TransportStream>,
{
    /// Error returned when connection fails
    type Error: std::error::Error + Send + Sync + 'static;

    /// The future type returned by this service
    type Future: Future<Output = Result<TransportStream, <Self as Transport>::Error>>
        + Send
        + 'static;

    /// Connect to a remote server and return a stream.
    fn connect(&mut self, uri: Uri) -> <Self as Transport>::Future;

    /// Poll the transport to see if it is ready to accept a new connection.
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Transport>::Error>>;
}

impl<T> Transport for T
where
    T: Service<Uri, Response = TransportStream>,
    T: Clone + Send + Sync + 'static,
    T::Error: std::error::Error + Send + Sync + 'static,
    T::Future: Send + 'static,
{
    type Error = T::Error;
    type Future = T::Future;

    fn connect(&mut self, uri: Uri) -> <Self as Service<Uri>>::Future {
        self.call(uri)
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Transport>::Error>> {
        Service::poll_ready(self, cx)
    }
}

/// A transport provides data transmission between two endpoints.
///
/// This transport uses braid to power the underlying
#[derive(Debug)]
pub struct TransportStream {
    stream: Stream,
    info: ConnectionInfo,
}

impl TransportStream {
    /// Create a new transport from a `braid::client::Stream`.
    pub async fn new(mut stream: Stream) -> io::Result<Self> {
        stream.finish_handshake().await?;

        let info = stream.info().await?;

        Ok(Self { stream, info })
    }

    pub(crate) fn info(&self) -> &ConnectionInfo {
        &self.info
    }

    pub(crate) fn host(&self) -> Option<&str> {
        self.info.authority.as_ref().map(|a| a.as_str())
    }
}

impl PoolableTransport for TransportStream {
    fn can_share(&self) -> bool {
        self.info.tls.as_ref().and_then(|tls| tls.alpn.as_ref())
            == Some(&braid::info::Protocol::Http(::http::Version::HTTP_2))
    }
}

impl From<TransportStream> for Stream {
    fn from(transport: TransportStream) -> Self {
        transport.stream
    }
}

/// Protocols (like HTTP) define how data is sent and received over a connection.
pub trait Protocol
where
    Self: Service<TransportStream, Response = Self::Connection>,
{
    /// Error returned when connection fails
    type Error: std::error::Error + Send + Sync + 'static;

    /// The type of connection returned by this service
    type Connection: Connection + PoolableConnection;

    /// The type of the handshake future
    type Future: Future<Output = Result<Self::Connection, <Self as Protocol>::Error>>
        + Send
        + 'static;

    /// Connect to a remote server and return a connection.
    fn connect(&mut self, transport: TransportStream) -> <Self as Protocol>::Future;

    /// Poll the protocol to see if it is ready to accept a new connection.
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Protocol>::Error>>;
}

impl<T, C> Protocol for T
where
    T: Service<TransportStream, Response = C> + Send + 'static,
    T::Error: std::error::Error + Send + Sync + 'static,
    T::Future: Send + 'static,
    C: Connection + PoolableConnection,
{
    type Error = T::Error;
    type Connection = C;
    type Future = T::Future;

    fn connect(&mut self, transport: TransportStream) -> <Self as Protocol>::Future {
        self.call(transport)
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Protocol>::Error>> {
        Service::poll_ready(self, cx)
    }
}

/// The HTTP protocol to use for a connection.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum HttpProtocol {
    /// Connect using HTTP/1.1
    Http1,

    /// Connect using HTTP/2
    Http2,
}

impl HttpProtocol {
    /// Does this protocol allow multiplexing?
    pub fn multiplex(&self) -> bool {
        matches!(self, Self::Http2)
    }
}

impl From<::http::Version> for HttpProtocol {
    fn from(version: ::http::Version) -> Self {
        match version {
            ::http::Version::HTTP_11 | ::http::Version::HTTP_10 => Self::Http1,
            ::http::Version::HTTP_2 => Self::Http2,
            _ => panic!("Unsupported HTTP protocol"),
        }
    }
}

/// A connection to a remote server which can send and recieve HTTP requests/responses.
///
/// Underneath, it may not use HTTP as the connection protocol, and it may use any appropriate
/// transport protocol to connect to the server.
pub trait Connection {
    /// The error type for this connection
    type Error: std::error::Error + Send + Sync + 'static;

    /// The future type returned by this service
    type Future: Future<Output = Result<::http::Response<Incoming>, Self::Error>> + Send + 'static;

    /// Send a request to the remote server and return the response.
    fn send_request(&mut self, request: arnold::Request) -> Self::Future;

    /// Poll the connection to see if it is ready to accept a new request.
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>>;

    /// Future which resolves when the connection is ready to accept a new request.
    fn when_ready(&mut self) -> BoxFuture<'_, Result<(), Self::Error>>
    where
        Self: Send,
    {
        futures_util::future::poll_fn(|cx| self.poll_ready(cx)).boxed()
    }

    /// What HTTP version is this connection using?
    fn version(&self) -> ::http::Version;
}
