use ::http::Uri;
use braid::client::Stream;
use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use std::future::Future;

use hyper::body::Incoming;
use tower::Service;

pub mod dns;
pub mod http;
pub mod tcp;

use crate::pool::Poolable;

pub use self::http::ConnectionError;
pub(crate) use self::http::HttpConnector;
pub(crate) use self::tcp::TcpConnectionConfig;
pub(crate) use self::tcp::TcpConnector;

/// Trait for types which implement a [tower::Service] appropriate for opening a
/// [Stream] to a [Uri].
pub trait Transport
where
    Self: Service<Uri, Response = Stream>,
{
}

impl<T> Transport for T
where
    T: Service<Uri, Response = Stream>,
    T::Error: std::error::Error + Send + Sync + 'static,
{
}

/// Trait for types which implement a [tower::Service] appropriate for connecting to a
/// [Uri].
pub trait Connect
where
    Self: Service<Uri>,
{
    /// Error returned when connection fails
    type Error: std::error::Error + Send + Sync + 'static;

    /// The type of connection returned by this service
    type Connection: Connection + Poolable;
}

impl<T, C> Connect for T
where
    T: Service<Uri, Response = C> + Send + 'static,
    T::Error: std::error::Error + Send + Sync + 'static,
    T::Future: Send + 'static,
    C: Connection + Poolable,
{
    type Error = T::Error;
    type Connection = C;
}

/// The HTTP protocol to use for a connection.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum ConnectionProtocol {
    /// Connect using HTTP/1.1
    Http1,

    /// Connect using HTTP/2
    #[allow(dead_code)]
    Http2,
}

impl ConnectionProtocol {
    #[allow(dead_code)]
    /// Does this protocol allow multiplexing?
    pub fn multiplex(&self) -> bool {
        matches!(self, Self::Http2)
    }
}

impl From<::http::Version> for ConnectionProtocol {
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
    type Error: std::error::Error + Send + Sync + 'static;
    type Future: Future<Output = Result<::http::Response<Incoming>, Self::Error>> + Send + 'static;

    fn send_request(&mut self, request: arnold::Request) -> Self::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>>;

    fn when_ready(&mut self) -> BoxFuture<'_, Result<(), Self::Error>>
    where
        Self: Send,
    {
        futures_util::future::poll_fn(|cx| self.poll_ready(cx)).boxed()
    }

    /// What HTTP version is this connection using?
    fn version(&self) -> ::http::Version;
}
