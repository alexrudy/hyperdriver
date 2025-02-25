//! Server-side connection builders for the HTTP2 protocol and the HTTP1 protocol.

use std::io;
use std::pin::Pin;

use crate::body::IncomingRequestService;
use crate::bridge::io::TokioIo;
use crate::bridge::service::TowerHyperFuture;
use crate::bridge::service::TowerHyperService;
use hyper::body::Incoming;
use hyper::rt::bounds::Http2ServerConnExec;
pub use hyper::server::conn::http1;
pub use hyper::server::conn::http2;
use thiserror::Error;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use crate::server::Protocol;
use crate::BoxError;
pub use acceptor::Acceptor;
#[cfg(feature = "stream")]
pub use acceptor::AcceptorCore;
pub use info::{
    ConnectionWithInfo, MakeServiceConnectionInfoLayer, MakeServiceConnectionInfoService,
};
pub use stream::{Accept, AcceptExt, AcceptOne, Stream};

mod acceptor;
/// HTTP connection builder with automatic protocol detection.
pub mod auto;
mod connecting;
pub(super) mod drivers;
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

/// A connection that can be gracefully shutdown.
pub trait Connection {
    /// Gracefully shutdown the connection.
    fn graceful_shutdown(self: Pin<&mut Self>);
}

impl<S, IO, BIn, BOut> Connection
    for http1::UpgradeableConnection<TokioIo<IO>, Adapted<S, BIn, BOut>>
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
        http1::UpgradeableConnection::graceful_shutdown(self)
    }
}

impl<S, IO, BIn, BOut> Protocol<S, IO, BIn> for http1::Builder
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
    type ResponseBody = BOut;
    type Connection = http1::UpgradeableConnection<TokioIo<IO>, Adapted<S, BIn, BOut>>;
    type Error = hyper::Error;

    fn serve_connection_with_upgrades(&self, stream: IO, service: S) -> Self::Connection
    where
        IO: AsyncRead + AsyncWrite + Unpin + 'static,
    {
        let conn = self.serve_connection(
            TokioIo::new(stream),
            TowerHyperService::new(IncomingRequestService::new(service)),
        );
        conn.with_upgrades()
    }
}

impl<S, IO, BIn, BOut, E> Connection for http2::Connection<TokioIo<IO>, Adapted<S, BIn, BOut>, E>
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
        http2::Connection::graceful_shutdown(self)
    }
}

impl<S, IO, BIn, BOut, E> Protocol<S, IO, BIn> for http2::Builder<E>
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
    type ResponseBody = BOut;
    type Connection = http2::Connection<TokioIo<IO>, Adapted<S, BIn, BOut>, E>;
    type Error = hyper::Error;

    fn serve_connection_with_upgrades(&self, stream: IO, service: S) -> Self::Connection {
        self.serve_connection(
            TokioIo::new(stream),
            TowerHyperService::new(IncomingRequestService::new(service)),
        )
    }
}
