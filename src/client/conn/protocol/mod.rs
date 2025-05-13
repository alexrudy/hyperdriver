//! Protocol describes how http requests and responses are transmitted over a connection.
//!
//! There are three protocols provided here: HTTP/1.1, HTTP/2, and an automatically
//! negotiated protocol which can be either HTTP/1.1 or HTTP/2 based on the connection
//! protocol and ALPN negotiation.

use std::future::Future;
use std::marker::PhantomData;

use http_body::Body;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tower::Service;

use self::future::HttpProtocolFuture;
use super::connection::ConnectionError;
use super::Connection;
use crate::bridge::io::TokioIo;
use crate::info::HasConnectionInfo;
use crate::BoxError;

pub mod auto;
pub mod info;
#[cfg(feature = "mocks")]
pub mod mock;
pub mod notify;
pub use hyper::client::conn::http1;
pub use hyper::client::conn::http2;

/// A request to establish a connection using a specific HTTP protocol
/// over a given transport.
#[derive(Debug)]
#[non_exhaustive]
pub struct ProtocolRequest<IO: HasConnectionInfo, B> {
    /// The transport to use for the connection
    pub transport: IO,

    /// The HTTP protocol to use for the connection
    pub version: HttpProtocol,

    _body: PhantomData<fn() -> B>,
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
pub trait Protocol<IO, B>
where
    IO: HasConnectionInfo,
    Self: Service<ProtocolRequest<IO, B>, Response = Self::Connection>,
{
    /// Error returned when connection fails
    type Error: std::error::Error + Send + Sync + 'static;

    /// The type of connection returned by this service
    type Connection: Connection<B>;

    /// The type of the handshake future
    type Future: Future<Output = Result<Self::Connection, <Self as Protocol<IO, B>>::Error>>
        + Send
        + 'static;

    /// Connect to a remote server and return a connection.
    ///
    /// The protocol version is provided to facilitate the correct handshake procedure.
    fn connect(
        &mut self,
        transport: IO,
        version: HttpProtocol,
    ) -> <Self as Protocol<IO, B>>::Future;

    /// Poll the protocol to see if it is ready to accept a new connection.
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Protocol<IO, B>>::Error>>;
}

impl<T, C, IO, B> Protocol<IO, B> for T
where
    IO: HasConnectionInfo,
    T: Service<ProtocolRequest<IO, B>, Response = C> + Send + 'static,
    T::Error: std::error::Error + Send + Sync + 'static,
    T::Future: Send + 'static,
    C: Connection<B>,
{
    type Error = T::Error;
    type Connection = C;
    type Future = T::Future;

    fn connect(
        &mut self,
        transport: IO,
        version: HttpProtocol,
    ) -> <Self as Protocol<IO, B>>::Future {
        self.call(ProtocolRequest {
            transport,
            version,
            _body: PhantomData,
        })
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Protocol<IO, B>>::Error>> {
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

/// Opaque future for protocols
mod future {
    use super::ConnectionError;
    use std::fmt;
    use std::future::Future;
    use std::pin::Pin;

    /// Opaque future for sending a request over a connection.
    pub struct HttpProtocolFuture<C> {
        inner: Pin<Box<dyn Future<Output = Result<C, ConnectionError>> + Send + 'static>>,
    }

    impl<C> fmt::Debug for HttpProtocolFuture<C> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("HttpProtocolFuture").finish()
        }
    }

    impl<C> HttpProtocolFuture<C> {
        pub(super) fn new<F>(future: F) -> Self
        where
            F: Future<Output = Result<C, ConnectionError>> + Send + 'static,
        {
            Self {
                inner: Box::pin(future),
            }
        }
    }

    impl<C> Future for HttpProtocolFuture<C> {
        type Output = Result<C, ConnectionError>;

        fn poll(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            self.inner.as_mut().poll(cx)
        }
    }
}

impl<IO, B> tower::Service<ProtocolRequest<IO, B>> for hyper::client::conn::http1::Builder
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    B: Body + Unpin + Send + 'static,
    <B as Body>::Data: Send,
    <B as Body>::Error: Into<BoxError>,
{
    type Response = hyper::client::conn::http1::SendRequest<B>;

    type Error = ConnectionError;
    type Future = HttpProtocolFuture<hyper::client::conn::http1::SendRequest<B>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: ProtocolRequest<IO, B>) -> Self::Future {
        let builder = std::mem::replace(self, self.clone());
        let stream = req.transport;

        // let info = stream.info();
        // let span = tracing::info_span!("connection", version=?http::Version::HTTP_11, peer=%info.remote_addr());

        HttpProtocolFuture::new(async move {
            let (sender, conn) = builder
                .handshake(TokioIo::new(stream))
                .await
                .map_err(|err| ConnectionError::Handshake(err.into()))?;

            tokio::spawn(async {
                if let Err(err) = conn.with_upgrades().await {
                    if err.is_user() {
                        tracing::error!(err = format!("{err:#}"), "h1 connection driver error");
                    } else {
                        tracing::debug!(err = format!("{err:#}"), "h1 connection driver error");
                    }
                }
            });
            Ok(sender)
        })
    }
}

impl<E, IO, BIn> tower::Service<ProtocolRequest<IO, BIn>> for hyper::client::conn::http2::Builder<E>
where
    E: hyper::rt::bounds::Http2ClientConnExec<BIn, TokioIo<IO>>
        + Unpin
        + Send
        + Sync
        + Clone
        + 'static,
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    BIn: Body + Unpin + Send + 'static,
    <BIn as Body>::Data: Send,
    <BIn as Body>::Error: Into<BoxError>,
{
    type Response = hyper::client::conn::http2::SendRequest<BIn>;

    type Error = ConnectionError;
    type Future = HttpProtocolFuture<hyper::client::conn::http2::SendRequest<BIn>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: ProtocolRequest<IO, BIn>) -> Self::Future {
        let builder = std::mem::replace(self, self.clone());
        let stream = req.transport;
        // let info = stream.info();
        // let span = tracing::info_span!("connection", version=?http::Version::HTTP_11, peer=%info.remote_addr());

        HttpProtocolFuture::new(async move {
            let (sender, conn) = builder
                .handshake(TokioIo::new(stream))
                .await
                .map_err(|err| ConnectionError::Handshake(err.into()))?;
            tokio::spawn(async {
                if let Err(err) = conn.await {
                    if err.is_user() {
                        tracing::error!(err = format!("{err:#}"), "h2 connection driver error");
                    } else {
                        tracing::debug!(err = format!("{err:#}"), "h2 connection driver error");
                    }
                }
            });
            Ok(sender)
        })
    }
}
