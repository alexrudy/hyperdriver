//! Server-side connection builders for the HTTP2 protocol and the HTTP1 protocol.

use std::io;
use std::pin::Pin;

pub use hyper::server::conn::http1;
pub use hyper::server::conn::http2;
use hyperdriver_body::AdaptIncomingService;
use hyperdriver_core::bridge::io::TokioIo;
use hyperdriver_core::bridge::rt::TokioExecutor;
use hyperdriver_core::bridge::service::TowerHyperService;
use thiserror::Error;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use crate::Protocol;
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

type Adapted<S> = TowerHyperService<AdaptIncomingService<S>>;

/// A connection that can be gracefully shutdown.
pub trait Connection {
    /// Gracefully shutdown the connection.
    fn graceful_shutdown(self: Pin<&mut Self>);
}

impl<S, IO> Connection for http1::UpgradeableConnection<TokioIo<IO>, Adapted<S>>
where
    S: tower::Service<hyperdriver_body::Request, Response = hyperdriver_body::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    fn graceful_shutdown(self: Pin<&mut Self>) {
        self.graceful_shutdown()
    }
}

impl<S, IO> Protocol<S, IO> for http1::Builder
where
    S: tower::Service<hyperdriver_body::Request, Response = hyperdriver_body::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Connection = http1::UpgradeableConnection<TokioIo<IO>, Adapted<S>>;
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

impl<S, IO> Connection for http2::Connection<TokioIo<IO>, Adapted<S>, TokioExecutor>
where
    S: tower::Service<hyperdriver_body::Request, Response = hyperdriver_body::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    fn graceful_shutdown(self: Pin<&mut Self>) {
        self.graceful_shutdown()
    }
}

impl<S, IO> Protocol<S, IO> for http2::Builder<TokioExecutor>
where
    S: tower::Service<hyperdriver_body::Request, Response = hyperdriver_body::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Connection = http2::Connection<TokioIo<IO>, Adapted<S>, TokioExecutor>;
    type Error = hyper::Error;

    fn serve_connection_with_upgrades(&self, stream: IO, service: S) -> Self::Connection {
        self.serve_connection(
            TokioIo::new(stream),
            TowerHyperService::new(AdaptIncomingService::new(service)),
        )
    }
}
