use std::future::Future;
use std::pin::Pin;

use braid::server::Stream;
use hyper::body::Incoming;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::{Builder, UpgradeableConnection},
    service::TowerToHyperService,
};
use ouroboros::self_referencing;

type Connection<'a, S> =
    UpgradeableConnection<'a, TokioIo<Stream>, TowerToHyperService<S>, TokioExecutor>;

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
                TowerToHyperService::new(service),
            ))
        })
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
    type Output = Result<(), Box<dyn std::error::Error + Send + Sync + 'static>>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.with_conn_mut(|conn| Pin::new(conn).poll(cx))
    }
}
