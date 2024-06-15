use std::future::Future;
use std::pin::Pin;
use std::task::ready;
use std::task::Poll;

use super::auto::Builder;
use super::auto::UpgradableConnection;
use super::ConnectionError;
use crate::body::AdaptIncomingService;
use crate::bridge::io::TokioIo;
use crate::bridge::rt::TokioExecutor;
use crate::bridge::service::TowerHyperService;
use ouroboros::self_referencing;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

type Connection<'a, S, IO> = UpgradableConnection<
    'a,
    TokioIo<IO>,
    TowerHyperService<AdaptIncomingService<S>>,
    TokioExecutor,
>;

/// Connection that handles the self-referential relationship between
/// the protocol and the connection.
#[self_referencing]
pub struct Connecting<S, IO>
where
    S: tower::Service<crate::body::Request, Response = crate::body::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    protocol: Builder<TokioExecutor>,

    #[borrows(protocol)]
    #[not_covariant]
    conn: Pin<Box<Connection<'this, S, IO>>>,
}

impl<S, IO> Connecting<S, IO>
where
    S: tower::Service<crate::body::Request, Response = crate::body::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    pub(crate) fn build(protocol: Builder<TokioExecutor>, service: S, stream: IO) -> Self {
        Self::new(protocol, move |protocol| {
            Box::pin(protocol.serve_connection_with_upgrades(
                TokioIo::new(stream),
                TowerHyperService::new(AdaptIncomingService::new(service)),
            ))
        })
    }
}

impl<S, IO> crate::server::Connection for Connecting<S, IO>
where
    S: tower::Service<crate::body::Request, Response = crate::body::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    fn graceful_shutdown(mut self: Pin<&mut Self>) {
        self.with_conn_mut(|conn| conn.as_mut().graceful_shutdown())
    }
}

impl<S, IO> Future for Connecting<S, IO>
where
    S: tower::Service<crate::body::Request, Response = crate::body::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
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
