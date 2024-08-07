//! Server-side connection builders for the HTTP2 protocol and the HTTP1 protocol.

use std::io;
use std::pin::Pin;

use crate::body::AdaptIncomingService;
use crate::bridge::io::TokioIo;
use crate::bridge::rt::TokioExecutor;
use crate::bridge::service::TowerHyperService;
pub use hyper::server::conn::http1;
pub use hyper::server::conn::http2;
use thiserror::Error;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use crate::server::Protocol;
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
mod info;
mod stream;
#[cfg(feature = "tls")]
pub mod tls;

/// Errors that can occur when handling connections.
#[derive(Debug, Error)]
pub enum ConnectionError {
    /// An error occurred while processing the connection in hyper.
    #[error(transparent)]
    Hyper(#[from] hyper::Error),

    /// An error occurred in the internal service used to handle requests.
    #[error("service: {0}")]
    Service(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// An error occurred while handling the protocol or upgrading the connection.
    #[error("protocol: {0}")]
    Protocol(#[source] io::Error),
}

type Adapted<S, BIn, BOut> = TowerHyperService<AdaptIncomingService<S, BIn, BOut>>;

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
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    BIn: From<hyper::body::Incoming>,
    BOut: http_body::Body + Send + 'static,
    BOut::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    fn graceful_shutdown(self: Pin<&mut Self>) {
        http1::UpgradeableConnection::graceful_shutdown(self)
    }
}

impl<S, IO, BIn, BOut> Protocol<S, IO, BIn> for http1::Builder
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    BIn: From<hyper::body::Incoming> + Send + 'static,
    BOut: http_body::Body + Send + 'static,
    BOut::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    BOut::Data: Send,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type ResponseBody = BOut;
    type Connection = http1::UpgradeableConnection<TokioIo<IO>, Adapted<S, BIn, BOut>>;
    type Error = hyper::Error;

    fn serve_connection_with_upgrades(&self, stream: IO, service: S) -> Self::Connection
    where
        IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let conn = self.serve_connection(
            TokioIo::new(stream),
            TowerHyperService::new(AdaptIncomingService::new(service)),
        );
        conn.with_upgrades()
    }
}

impl<S, IO, BIn, BOut> Connection
    for http2::Connection<TokioIo<IO>, Adapted<S, BIn, BOut>, TokioExecutor>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    BIn: From<hyper::body::Incoming> + 'static,
    BOut: http_body::Body + Send + 'static,
    BOut::Data: Send + 'static,
    BOut::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    fn graceful_shutdown(self: Pin<&mut Self>) {
        http2::Connection::graceful_shutdown(self)
    }
}

impl<S, IO, BIn, BOut> Protocol<S, IO, BIn> for http2::Builder<TokioExecutor>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    BIn: From<hyper::body::Incoming> + Send + 'static,
    BOut: http_body::Body + Send + 'static,
    BOut::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    BOut::Data: Send + 'static,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type ResponseBody = BOut;
    type Connection = http2::Connection<TokioIo<IO>, Adapted<S, BIn, BOut>, TokioExecutor>;
    type Error = hyper::Error;

    fn serve_connection_with_upgrades(&self, stream: IO, service: S) -> Self::Connection {
        self.serve_connection(
            TokioIo::new(stream),
            TowerHyperService::new(AdaptIncomingService::new(service)),
        )
    }
}
