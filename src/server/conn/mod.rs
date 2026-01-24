//! Server-side connection builders for the HTTP2 protocol and the HTTP1 protocol.

use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use crate::body::IncomingRequestService;
use crate::bridge::io::TokioIo;
use crate::bridge::service::TowerHyperFuture;
use crate::bridge::service::TowerHyperService;
pub use chateau::server::Connection;
use hyper::body::Incoming;
use hyper::rt::bounds::Http2ServerConnExec;
pub use hyper::server::conn::http1;
pub use hyper::server::conn::http2;
use thiserror::Error;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use crate::BoxError;
use crate::server::Protocol;
pub use acceptor::Acceptor;
#[cfg(feature = "stream")]
pub use acceptor::AcceptorCore;
pub use chateau::server::{Accept, conn::AcceptExt, conn::AcceptOne};
pub use info::{
    ConnectionWithInfo, MakeServiceConnectionInfoLayer, MakeServiceConnectionInfoService,
};
pub use stream::Stream;

mod acceptor;
/// HTTP connection builder with automatic protocol detection.
pub mod auto;
mod connecting;
mod info;
mod stream;
#[cfg(feature = "tls")]
pub mod tls;

/// Errors that can occur when handling connections.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConnectionError {
    /// An error occurred while processing the connection in hyper.
    #[error(transparent)]
    Hyper(#[from] hyper::Error),

    /// An error occurred in the internal service used to handle requests.
    #[error("service: {0}")]
    Service(#[source] BoxError),

    /// An error occurred while handling the protocol or upgrading the connection.
    #[error("protocol: {0}")]
    Protocol(#[source] io::Error),
}

type Adapted<S, BIn, BOut> = TowerHyperService<IncomingRequestService<S, BIn, BOut>>;

/// An HTTP/1 server connection which supports upgrades.
#[pin_project::pin_project]
pub struct HTTP1Connection<S, IO, BIn, BOut>(
    #[pin] http1::UpgradeableConnection<TokioIo<IO>, Adapted<S, BIn, BOut>>,
)
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    BIn: From<hyper::body::Incoming>,
    BOut: http_body::Body + Send + 'static,
    BOut::Error: Into<BoxError>,
    IO: AsyncRead + AsyncWrite + Unpin + 'static;

impl<S, IO, BIn, BOut> fmt::Debug for HTTP1Connection<S, IO, BIn, BOut>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    BIn: From<hyper::body::Incoming>,
    BOut: http_body::Body + Send + 'static,
    BOut::Error: Into<BoxError>,
    IO: AsyncRead + AsyncWrite + Unpin + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("HTTP1Connection").finish()
    }
}

impl<S, IO, BIn, BOut> Connection for HTTP1Connection<S, IO, BIn, BOut>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    BIn: From<hyper::body::Incoming>,
    BOut: http_body::Body + Send + 'static,
    BOut::Error: Into<BoxError>,
    IO: AsyncRead + AsyncWrite + Unpin + 'static,
{
    fn graceful_shutdown(self: Pin<&mut Self>) {
        http1::UpgradeableConnection::graceful_shutdown(self.project().0)
    }
}

impl<S, IO, BIn, BOut> Future for HTTP1Connection<S, IO, BIn, BOut>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    BIn: From<hyper::body::Incoming>,
    BOut: http_body::Body + Send + 'static,
    BOut::Error: Into<BoxError>,
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Output = Result<(), hyper::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().0.poll(cx)
    }
}

/// An HTTP/1 Server connection builder that wraps the hyper-equivalent.
#[derive(Debug)]
pub struct Http1Builder(http1::Builder);

impl Http1Builder {
    /// Create a new HTTP/1 builder
    pub fn new() -> Self {
        Self(http1::Builder::new())
    }
}

impl Default for Http1Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl From<http1::Builder> for Http1Builder {
    fn from(builder: http1::Builder) -> Self {
        Http1Builder(builder)
    }
}

impl<S, IO, BIn, BOut> Protocol<S, IO, http::Request<BIn>> for Http1Builder
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    BIn: From<hyper::body::Incoming> + Send + 'static,
    BOut: http_body::Body + Send + 'static,
    BOut::Error: Into<BoxError>,
    BOut::Data: Send,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Response = http::Response<BOut>;
    type Connection = HTTP1Connection<S, IO, BIn, BOut>;
    type Error = hyper::Error;

    fn serve_connection(&self, stream: IO, service: S) -> Self::Connection {
        let conn = self.0.serve_connection(
            TokioIo::new(stream),
            TowerHyperService::new(IncomingRequestService::new(service)),
        );
        HTTP1Connection(conn.with_upgrades())
    }
}

/// An HTTP/2 connection which wraps the hyper equivalent.
#[pin_project::pin_project]
#[derive(Debug)]
pub struct Http2Connection<S, IO, BIn, BOut, E>(
    #[pin] http2::Connection<TokioIo<IO>, Adapted<S, BIn, BOut>, E>,
)
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + 'static,
    S::Future: 'static,
    S::Error: Into<BoxError>,
    BIn: From<hyper::body::Incoming> + 'static,
    BOut: http_body::Body + Send + 'static,
    BOut::Data: Send + 'static,
    BOut::Error: Into<BoxError>,
    IO: AsyncRead + AsyncWrite + Unpin + 'static,
    E: Http2ServerConnExec<
            TowerHyperFuture<IncomingRequestService<S, BIn, BOut>, http::Request<Incoming>>,
            BOut,
        > + Clone
        + Send
        + 'static;

impl<S, IO, BIn, BOut, E> Connection for Http2Connection<S, IO, BIn, BOut, E>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + 'static,
    S::Future: 'static,
    S::Error: Into<BoxError>,
    BIn: From<hyper::body::Incoming> + 'static,
    BOut: http_body::Body + Send + 'static,
    BOut::Data: Send + 'static,
    BOut::Error: Into<BoxError>,
    IO: AsyncRead + AsyncWrite + Unpin + 'static,
    E: Http2ServerConnExec<
            TowerHyperFuture<IncomingRequestService<S, BIn, BOut>, http::Request<Incoming>>,
            BOut,
        > + Clone
        + Send
        + 'static,
{
    fn graceful_shutdown(self: Pin<&mut Self>) {
        tracing::trace!("requesting shutdown for http/2 connection");
        http2::Connection::graceful_shutdown(self.project().0)
    }
}

impl<S, IO, BIn, BOut, E> Future for Http2Connection<S, IO, BIn, BOut, E>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + 'static,
    S::Future: 'static,
    S::Error: Into<BoxError>,
    BIn: From<hyper::body::Incoming> + 'static,
    BOut: http_body::Body + Send + 'static,
    BOut::Data: Send + 'static,
    BOut::Error: Into<BoxError>,
    IO: AsyncRead + AsyncWrite + Unpin + 'static,
    E: Http2ServerConnExec<
            TowerHyperFuture<IncomingRequestService<S, BIn, BOut>, http::Request<Incoming>>,
            BOut,
        > + Clone
        + Send
        + 'static,
{
    type Output = Result<(), hyper::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().0.poll(cx)
    }
}

/// An HTTP/2 connection builder which wraps the hyper equivalent.
#[derive(Debug, Clone)]
pub struct Http2Builder<E>(http2::Builder<E>);

impl<E> Http2Builder<E> {
    /// Create a new HTTP2 builder from an executor.
    pub fn new(executor: E) -> Self {
        Http2Builder(http2::Builder::new(executor))
    }
}

impl<E> From<http2::Builder<E>> for Http2Builder<E> {
    fn from(builder: http2::Builder<E>) -> Self {
        Http2Builder(builder)
    }
}

impl<S, IO, BIn, BOut, E> Protocol<S, IO, http::Request<BIn>> for Http2Builder<E>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + 'static,
    S::Future: 'static,
    S::Error: Into<BoxError>,
    BIn: http_body::Body + From<hyper::body::Incoming> + Send + 'static,
    BOut: http_body::Body + Send + 'static,
    BOut::Error: Into<BoxError>,
    BOut::Data: Send + 'static,
    IO: AsyncRead + AsyncWrite + Unpin + 'static,
    E: Http2ServerConnExec<
            TowerHyperFuture<IncomingRequestService<S, BIn, BOut>, http::Request<Incoming>>,
            BOut,
        > + Clone
        + Send
        + 'static,
{
    type Response = http::Response<BOut>;
    type Connection = Http2Connection<S, IO, BIn, BOut, E>;
    type Error = hyper::Error;

    fn serve_connection(&self, stream: IO, service: S) -> Self::Connection {
        Http2Connection(self.0.serve_connection(
            TokioIo::new(stream),
            TowerHyperService::new(IncomingRequestService::new(service)),
        ))
    }
}
