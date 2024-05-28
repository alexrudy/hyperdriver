use ::http::Uri;
use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use pin_project::pin_project;
use std::future::Future;
use std::io;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use hyper::body::Incoming;
use tower::Service;

#[cfg(feature = "stream")]
pub mod dns;

#[cfg(feature = "stream")]
pub mod duplex;
pub mod http;

#[cfg(feature = "stream")]
pub mod tcp;

use crate::client::pool::PoolableTransport;
#[cfg(feature = "stream")]
use crate::stream::client::Stream;
use crate::stream::info::ConnectionInfo;
use crate::stream::info::HasConnectionInfo;

pub use self::http::ConnectionError;

#[cfg(feature = "stream")]
pub(crate) use self::tcp::TcpConnectionConfig;
#[cfg(feature = "stream")]
pub(crate) use self::tcp::TcpConnector;

/// A transport provides data transmission between two endpoints.
pub trait Transport: Clone + Send {
    /// The type of IO stream used by this transport
    type IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + 'static;

    /// Error returned when connection fails
    type Error: std::error::Error + Send + Sync + 'static;

    /// The future type returned by this service
    type Future: Future<Output = Result<TransportStream<Self::IO>, <Self as Transport>::Error>>
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

impl<T, IO> Transport for T
where
    T: Service<Uri, Response = TransportStream<IO>>,
    T: Clone + Send + Sync + 'static,
    T::Error: std::error::Error + Send + Sync + 'static,
    T::Future: Send + 'static,
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + 'static,
{
    type IO = IO;
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
#[pin_project]
pub struct TransportStream<IO>
where
    IO: HasConnectionInfo,
{
    #[pin]
    stream: IO,
    info: ConnectionInfo<IO::Addr>,
}

impl<IO> TransportStream<IO>
where
    IO: HasConnectionInfo,
{
    #[cfg_attr(not(feature = "tls"), allow(dead_code))]
    pub(crate) fn info(&self) -> &ConnectionInfo<IO::Addr> {
        &self.info
    }

    pub(crate) fn host(&self) -> Option<&str> {
        self.info.authority.as_ref().map(|a| a.as_str())
    }

    pub(crate) fn into_inner(self) -> IO {
        self.stream
    }
}

#[cfg(feature = "stream")]
impl TransportStream<Stream> {
    /// Create a new transport from a `crate::stream::client::Stream`.
    #[cfg_attr(not(feature = "tls"), allow(unused_mut))]
    pub async fn new(mut stream: Stream) -> io::Result<Self> {
        #[cfg(feature = "tls")]
        stream.finish_handshake().await?;

        let info = stream.info();

        Ok(Self { stream, info })
    }
}

impl<IO> PoolableTransport for TransportStream<IO>
where
    IO: HasConnectionInfo + Unpin + Send + 'static,
    IO::Addr: Send,
{
    #[cfg(feature = "tls")]
    fn can_share(&self) -> bool {
        self.info.tls.as_ref().and_then(|tls| tls.alpn.as_ref())
            == Some(&crate::stream::info::Protocol::Http(
                ::http::Version::HTTP_2,
            ))
    }

    #[cfg(not(feature = "tls"))]
    fn can_share(&self) -> bool {
        false
    }
}

impl<IO> AsyncRead for TransportStream<IO>
where
    IO: HasConnectionInfo + AsyncRead,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        self.project().stream.poll_read(cx, buf)
    }
}

impl<IO> AsyncWrite for TransportStream<IO>
where
    IO: HasConnectionInfo + AsyncWrite,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, io::Error>> {
        self.project().stream.poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), io::Error>> {
        self.project().stream.poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), io::Error>> {
        self.project().stream.poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_write_vectored(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> std::task::Poll<Result<usize, io::Error>> {
        self.project().stream.poll_write_vectored(cx, bufs)
    }
}

/// Protocols (like HTTP) define how data is sent and received over a connection.
pub trait Protocol<IO>
where
    IO: HasConnectionInfo,
    Self: Service<TransportStream<IO>, Response = Self::Connection>,
{
    /// Error returned when connection fails
    type Error: std::error::Error + Send + Sync + 'static;

    /// The type of connection returned by this service
    type Connection: Connection;

    /// The type of the handshake future
    type Future: Future<Output = Result<Self::Connection, <Self as Protocol<IO>>::Error>>
        + Send
        + 'static;

    /// Connect to a remote server and return a connection.
    fn connect(&mut self, transport: TransportStream<IO>) -> <Self as Protocol<IO>>::Future;

    /// Poll the protocol to see if it is ready to accept a new connection.
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Protocol<IO>>::Error>>;
}

impl<T, C, IO> Protocol<IO> for T
where
    IO: HasConnectionInfo,
    T: Service<TransportStream<IO>, Response = C> + Send + 'static,
    T::Error: std::error::Error + Send + Sync + 'static,
    T::Future: Send + 'static,
    C: Connection,
{
    type Error = T::Error;
    type Connection = C;
    type Future = T::Future;

    fn connect(&mut self, transport: TransportStream<IO>) -> <Self as Protocol<IO>>::Future {
        self.call(transport)
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Protocol<IO>>::Error>> {
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
    fn send_request(&mut self, request: crate::body::Request) -> Self::Future;

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
