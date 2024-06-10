//! Client connection types.
//!
//! A client connection is composed of a transport, a protocol, and a connection, which each serve a
//! different purpose in the client connection lifecycle.
//!
//! ## Transport
//!
//! The transport is responsible for establishing a connection to a remote server, shuffling bytes back
//! and forth, and handling the low-level details of the connection. Transports implement the [`Transport`]
//! trait, effecitively making them a service which accepts a URI and returns a bidirectional stream.
//!
//! Two builtin transports are provided:
//! - [`TcpConnector`]: Connects to a remote server over TCP/IP. This is the default transport, and what
//!     usually powers HTTP connections.
//! - [`DuplexTransport`][duplex::DuplexTransport]: Connects to a remote server over a duplex stream, which
//!     is an in-memory stream that can be used for testing or other purposes.
//!
//! ## Protocol
//!
//! The protocol is responsible for encoding and decoding request and response objects. Usually, this means
//! HTTP/1.1 or HTTP/2, but it could be any protocol which sends and receives data over a connection.
//!
//! Protocols implement the [`Protocol`] trait, which is a service that accepts a [`ProtocolRequest`] and
//! returns a connection. The connection is responsible for sending and receiving HTTP requests and responses.
//!
//! ## Connection
//!
//! The connection is responsible for sending and receiving HTTP requests and responses. It is the highest
//! level of abstraction in the client connection stack, and is the  part of the stack which accepts requests.
//!
//! Connections implement the [`Connection`] trait, which is a service that accepts a request and returns a
//! future which resolves to a response. The connection is responsible for encoding and decoding the request
//! and response objects, and for sending and receiving the data over the transport.

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

pub mod dns;

#[cfg(feature = "stream")]
pub mod duplex;
pub mod http;

pub mod tcp;

use crate::client::pool::PoolableTransport;
#[cfg(feature = "stream")]
use crate::stream::client::Stream;
#[cfg(feature = "stream")]
use crate::stream::info::BraidAddr;
use crate::stream::info::ConnectionInfo;
use crate::stream::info::HasConnectionInfo;

pub use self::http::ConnectionError;

pub use self::tcp::TcpConnectionConfig;
pub use self::tcp::TcpConnector;

/// A transport provides data transmission between two endpoints.
///
/// To implement a transport stream, implement a [`tower::Service`] which accepts a URI and returns a
/// [`TransportStream`]. [`TransportStream`] is a wrapper around an IO stream which provides additional
/// information about the connection, such as the remote address and the protocol being used. The underlying
/// IO stream must implement [`tokio::io::AsyncRead`] and [`tokio::io::AsyncWrite`].
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

/// A wrapper around an IO stream which provides additional information about the connection.
///
/// This is used to attach [`ConnectionInfo`] to an arbitrary IO stream. IO streams must implement
/// [`tokio::io::AsyncRead`] and [`tokio::io::AsyncWrite`] to be functional.
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
    /// Create a new transport from an IO stream.
    pub fn new(stream: IO) -> Self {
        let info = stream.info();
        Self { stream, info }
    }

    #[cfg_attr(not(feature = "tls"), allow(dead_code))]
    pub(crate) fn info(&self) -> &ConnectionInfo<IO::Addr> {
        &self.info
    }

    pub(crate) fn host(&self) -> Option<&str> {
        self.info.authority.as_ref().map(|a| a.as_str())
    }

    /// Reduce the transport to its inner IO stream.
    pub fn into_inner(self) -> IO {
        self.stream
    }

    /// Map the inner IO stream to a new IO stream.
    pub fn map<F, U>(self, f: F) -> TransportStream<U>
    where
        F: FnOnce(IO) -> U,
        U: HasConnectionInfo,
        IO::Addr: Into<U::Addr>,
    {
        TransportStream {
            stream: f(self.stream),
            info: self.info.map(Into::into),
        }
    }
}

#[cfg(feature = "stream")]
impl TransportStream<Stream> {
    /// Create a new transport from a `crate::stream::client::Stream`.
    #[cfg_attr(not(feature = "tls"), allow(unused_mut))]
    pub async fn new_stream(mut stream: Stream) -> io::Result<Self> {
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

/// A request to establish a connection using a specific HTTP protocol
/// over a given transport.
#[derive(Debug)]
pub struct ProtocolRequest<IO: HasConnectionInfo> {
    /// The transport to use for the connection
    pub transport: TransportStream<IO>,

    /// The HTTP protocol to use for the connection
    pub version: HttpProtocol,
}

/// Protocols (like HTTP) define how data is sent and received over a connection.
///
/// A protocol is a service which accepts a [`ProtocolRequest`] and returns a connection.
///
/// The request contains a transport stream and the HTTP protocol to use for the connection.
///
/// The connection is responsible for sending and receiving HTTP requests and responses.
///
///
pub trait Protocol<IO>
where
    IO: HasConnectionInfo,
    Self: Service<ProtocolRequest<IO>, Response = Self::Connection>,
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
    ///
    /// The protocol version is provided to facilitate the correct handshake procedure.
    fn connect(
        &mut self,
        transport: TransportStream<IO>,
        version: HttpProtocol,
    ) -> <Self as Protocol<IO>>::Future;

    /// Poll the protocol to see if it is ready to accept a new connection.
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Protocol<IO>>::Error>>;
}

impl<T, C, IO> Protocol<IO> for T
where
    IO: HasConnectionInfo,
    T: Service<ProtocolRequest<IO>, Response = C> + Send + 'static,
    T::Error: std::error::Error + Send + Sync + 'static,
    T::Future: Send + 'static,
    C: Connection,
{
    type Error = T::Error;
    type Connection = C;
    type Future = T::Future;

    fn connect(
        &mut self,
        transport: TransportStream<IO>,
        version: HttpProtocol,
    ) -> <Self as Protocol<IO>>::Future {
        self.call(ProtocolRequest { transport, version })
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Protocol<IO>>::Error>> {
        Service::poll_ready(self, cx)
    }
}

/// The HTTP protocol to use for a connection.
///
/// This differs from the HTTP version in that it is constrained to the two flavors of HTTP
/// protocol, HTTP/1.1 and HTTP/2. HTTP/3 is not yet supported. HTTP/0.9 and HTTP/1.0 are
/// supported by HTTP/1.1.
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

    /// HTTP Version
    ///
    /// Convert the protocol to an HTTP version.
    ///
    /// For HTTP/1.1, this returns `::http::Version::HTTP_11`.
    /// For HTTP/2, this returns `::http::Version::HTTP_2`.
    pub fn version(&self) -> ::http::Version {
        match self {
            Self::Http1 => ::http::Version::HTTP_11,
            Self::Http2 => ::http::Version::HTTP_2,
        }
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

#[cfg(feature = "stream")]
/// A transport which can be converted into a stream.
#[derive(Debug, Clone)]
pub struct IntoStream<T> {
    transport: T,
}

#[cfg(feature = "stream")]
impl<T> IntoStream<T> {
    /// Create a new `IntoStream` transport.
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

#[cfg(feature = "stream")]
impl<T> Service<Uri> for IntoStream<T>
where
    T: Transport,
    T::IO: Into<Stream> + AsyncRead + AsyncWrite + Unpin + Send + 'static,
    <<T as Transport>::IO as HasConnectionInfo>::Addr: Into<BraidAddr>,
{
    type Response = TransportStream<Stream>;
    type Error = T::Error;
    type Future = fut::ConnectFuture<T>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.transport.poll_ready(cx)
    }

    fn call(&mut self, req: Uri) -> Self::Future {
        fut::ConnectFuture::new(self.transport.connect(req))
    }
}

#[cfg(feature = "stream")]
/// Extension trait for Transports to provide a method to convert them into a transport
/// which uses a hyperdriver Braided stream.
pub trait TransportExt: Transport {
    /// Wrap the transport in a converter which produces a Stream
    fn into_stream(self) -> IntoStream<Self>
    where
        Self::IO: Into<Stream> + AsyncRead + AsyncWrite + Unpin + Send + 'static,
        <<Self as Transport>::IO as HasConnectionInfo>::Addr: Into<BraidAddr>,
    {
        IntoStream::new(self)
    }
}

#[cfg(feature = "stream")]
impl<T> TransportExt for T where T: Transport {}

#[cfg(feature = "stream")]
mod fut {

    use pin_project::pin_project;

    use super::{Transport, TransportStream};
    use crate::stream::{
        client::Stream,
        info::{BraidAddr, HasConnectionInfo},
    };

    /// Future returned by `IntoStream` transports.
    #[pin_project]
    #[derive(Debug)]
    pub struct ConnectFuture<T>
    where
        T: Transport,
    {
        #[pin]
        future: T::Future,
    }

    impl<T> ConnectFuture<T>
    where
        T: Transport,
    {
        pub(super) fn new(future: T::Future) -> Self {
            Self { future }
        }
    }

    impl<T> std::future::Future for ConnectFuture<T>
    where
        T: Transport,
        T::IO: Into<Stream>,
        <<T as Transport>::IO as HasConnectionInfo>::Addr: Into<BraidAddr>,
    {
        type Output = Result<TransportStream<Stream>, T::Error>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            self.project()
                .future
                .poll(cx)
                .map_ok(|io| io.map(Into::into))
        }
    }
}
