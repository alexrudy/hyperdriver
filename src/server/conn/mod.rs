//! Server-side connection builders for the HTTP2 protocol and the HTTP1 protocol.

use std::pin::Pin;

use crate::bridge::io::TokioIo;
use crate::bridge::rt::TokioExecutor;
use crate::bridge::service::TowerHyperService;
pub use hyper::server::conn::http1;
pub use hyper::server::conn::http2;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use crate::server::Protocol;

/// HTTP connection builder with automatic protocol detection.
pub mod auto;
mod connecting;

/// A connection that can be gracefully shutdown.
pub trait Connection {
    /// Gracefully shutdown the connection.
    fn graceful_shutdown(self: Pin<&mut Self>);
}

impl<S, IO> Connection for http1::UpgradeableConnection<TokioIo<IO>, TowerHyperService<S>>
where
    S: tower::Service<http::Request<hyper::body::Incoming>, Response = crate::body::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    fn graceful_shutdown(self: Pin<&mut Self>) {
        self.graceful_shutdown()
    }
}

impl<S, IO> Protocol<S, IO> for http1::Builder
where
    S: tower::Service<http::Request<hyper::body::Incoming>, Response = crate::body::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Connection = http1::UpgradeableConnection<TokioIo<IO>, TowerHyperService<S>>;
    type Error = hyper::Error;

    fn serve_connection_with_upgrades(&self, stream: IO, service: S) -> Self::Connection
    where
        IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let conn = self.serve_connection(TokioIo::new(stream), TowerHyperService::new(service));
        conn.with_upgrades()
    }
}

impl<S, IO> Connection for http2::Connection<TokioIo<IO>, TowerHyperService<S>, TokioExecutor>
where
    S: tower::Service<http::Request<hyper::body::Incoming>, Response = crate::body::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    fn graceful_shutdown(self: Pin<&mut Self>) {
        self.graceful_shutdown()
    }
}

impl<S, IO> Protocol<S, IO> for http2::Builder<TokioExecutor>
where
    S: tower::Service<http::Request<hyper::body::Incoming>, Response = crate::body::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Connection = http2::Connection<TokioIo<IO>, TowerHyperService<S>, TokioExecutor>;
    type Error = hyper::Error;

    fn serve_connection_with_upgrades(&self, stream: IO, service: S) -> Self::Connection {
        self.serve_connection(TokioIo::new(stream), TowerHyperService::new(service))
    }
}
