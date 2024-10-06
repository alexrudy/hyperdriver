use std::future::Future;
use std::pin::Pin;
use std::task::ready;
use std::task::Poll;

use http;
use ouroboros::self_referencing;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use super::auto::Builder;
use super::auto::UpgradableConnection;
use super::ConnectionError;
use crate::body::IncomingRequestService;
use crate::bridge::io::TokioIo;
use crate::bridge::rt::TokioExecutor;
use crate::bridge::service::TowerHyperService;

type Connection<'a, S, IO, BIn, BOut> = UpgradableConnection<
    'a,
    TokioIo<IO>,
    TowerHyperService<IncomingRequestService<S, BIn, BOut>>,
    TokioExecutor,
>;

/// Connection that handles the self-referential relationship between
/// the protocol and the connection.
#[self_referencing]
pub struct Connecting<S, IO, BIn, BOut>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    BIn: From<hyper::body::Incoming>,
    BOut: http_body::Body,
{
    protocol: Builder<TokioExecutor>,

    #[borrows(protocol)]
    #[not_covariant]
    conn: Pin<Box<Connection<'this, S, IO, BIn, BOut>>>,
}

impl<S, IO, BIn, BOut> Connecting<S, IO, BIn, BOut>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    BIn: From<hyper::body::Incoming> + 'static,
    BOut: http_body::Body + 'static,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    pub(crate) fn build(protocol: Builder<TokioExecutor>, service: S, stream: IO) -> Self {
        Self::new(protocol, move |protocol| {
            Box::pin(protocol.serve_connection_with_upgrades(
                TokioIo::new(stream),
                TowerHyperService::new(IncomingRequestService::new(service)),
            ))
        })
    }
}

impl<S, IO, BIn, BOut> crate::server::Connection for Connecting<S, IO, BIn, BOut>
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
    fn graceful_shutdown(mut self: Pin<&mut Self>) {
        self.with_conn_mut(|conn| conn.as_mut().graceful_shutdown())
    }
}

impl<S, IO, BIn, BOut> Future for Connecting<S, IO, BIn, BOut>
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
    type Output = Result<(), ConnectionError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match ready!(self.with_conn_mut(|conn| Pin::new(conn).poll(cx))) {
            Ok(()) => Poll::Ready(Ok(())),
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}
