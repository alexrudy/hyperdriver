use std::future::Future;
use std::pin::Pin;

use super::auto::Builder;
use super::auto::UpgradableConnection;
use braid::server::Stream;
use bridge::io::TokioIo;
use bridge::rt::TokioExecutor;
use bridge::service::TowerHyperService;
use hyper::body::Incoming;
use ouroboros::self_referencing;
use tower::BoxError;

type Connection<'a, S> =
    UpgradableConnection<'a, TokioIo<Stream>, TowerHyperService<S>, TokioExecutor>;

#[self_referencing]
pub struct Connecting<S>
where
    S: tower::Service<http::Request<Incoming>, Response = arnold::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    protocol: Builder<TokioExecutor>,

    #[borrows(protocol)]
    #[not_covariant]
    conn: Pin<Box<Connection<'this, S>>>,
}

impl<S> Connecting<S>
where
    S: tower::Service<http::Request<Incoming>, Response = arnold::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    pub(crate) fn build(protocol: Builder<TokioExecutor>, service: S, stream: Stream) -> Self {
        Self::new(protocol, move |protocol| {
            Box::pin(protocol.serve_connection_with_upgrades(
                TokioIo::new(stream),
                TowerHyperService::new(service),
            ))
        })
    }
}

impl<S> crate::Connection<BoxError> for Connecting<S>
where
    S: tower::Service<http::Request<Incoming>, Response = arnold::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    fn graceful_shutdown(mut self: Pin<&mut Self>) {
        self.with_conn_mut(|conn| conn.as_mut().graceful_shutdown())
    }
}

impl<S> Future for Connecting<S>
where
    S: tower::Service<http::Request<Incoming>, Response = arnold::Response>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    type Output = Result<(), BoxError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.with_conn_mut(|conn| Pin::new(conn).poll(cx))
    }
}
