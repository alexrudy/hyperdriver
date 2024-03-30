//! Server-side connection builders for the HTTP2 protocol and the HTTP1 protocol.

use std::future::Future;
use std::pin::Pin;

use braid::server::Stream;
use bridge::io::TokioIo;
use bridge::rt::TokioExecutor;
use bridge::service::TowerHyperService;
pub use hyper::server::conn::http1;
pub use hyper::server::conn::http2;

use crate::Protocol;

/// HTTP connection builder with automatic protocol detection.
pub mod auto;
mod connecting;

type Error = Box<dyn std::error::Error + Send + Sync>;

/// A connection that can be gracefully shutdown.
pub trait Connection: Future<Output = Result<(), Error>> {
    /// Gracefully shutdown the connection.
    fn graceful_shutdown(self: Pin<&mut Self>);
}

impl<S> Protocol<S> for http1::Builder
where
    S: tower::Service<http::Request<hyper::body::Incoming>, Response = arnold::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    type Connection = http1::UpgradeableConnection<TokioIo<Stream>, TowerHyperService<S>>;
    type Error = hyper::Error;

    fn serve_connection_with_upgrades(&self, stream: Stream, service: S) -> Self::Connection {
        let conn = self.serve_connection(TokioIo::new(stream), TowerHyperService::new(service));
        conn.with_upgrades()
    }
}

impl<S> Protocol<S> for http2::Builder<TokioExecutor>
where
    S: tower::Service<http::Request<hyper::body::Incoming>, Response = arnold::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    type Connection = http2::Connection<TokioIo<Stream>, TowerHyperService<S>, TokioExecutor>;
    type Error = hyper::Error;

    fn serve_connection_with_upgrades(&self, stream: Stream, service: S) -> Self::Connection {
        self.serve_connection(TokioIo::new(stream), TowerHyperService::new(service))
    }
}
