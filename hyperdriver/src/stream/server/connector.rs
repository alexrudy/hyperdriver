//! Tower middleware for collecting connection information after a handshake has been completed.
//!
//! This middleware applies to the request stack, but recieves the connection info from the acceptor stack.

use std::{
    convert::Infallible,
    fmt,
    future::{ready, Ready},
    task::Poll,
};

use futures_core::future::BoxFuture;
use hyper::{Request, Response};
use tower::{Layer, Service};
use tracing::Instrument;

use super::ConnectionInfoState;
use super::Stream;
use crate::{polled_span, stream::info::Connection as HasConnectionInfo};

/// A middleware which adds connection information to the request extensions.
#[derive(Debug, Clone)]
pub struct StartConnectionInfoLayer;

impl<S> Layer<S> for StartConnectionInfoLayer {
    type Service = StartConnectionInfoService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        StartConnectionInfoService::new(inner)
    }
}

/// A service which adds connection information to the request extensions.
#[derive(Debug, Clone)]
pub struct StartConnectionInfoService<C> {
    inner: C,
}

impl<C> StartConnectionInfoService<C> {
    /// Create a new `StartConnectionInfoService` wrapping `inner` service,
    /// and applying `info` to the request extensions.
    pub fn new(inner: C) -> Self {
        Self { inner }
    }
}

impl<C, IO> Service<&Stream<IO>> for StartConnectionInfoService<C>
where
    C: Clone + Send + 'static,
    IO: HasConnectionInfo + Send + 'static,
{
    type Response = Connection<C>;

    type Error = Infallible;

    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, stream: &Stream<IO>) -> Self::Future {
        let inner = self.inner.clone();
        let info = stream.info.clone();
        ready(Ok(Connection { inner, info }))
    }
}

/// Interior service which adds connection information to the request extensions.
///
/// This service wraps the request/response service, not the connector service.
pub struct Connection<S> {
    inner: S,
    info: ConnectionInfoState,
}

impl<S, BIn, BOut> Service<Request<BIn>> for Connection<S>
where
    S: Service<Request<BIn>, Response = Response<BOut>> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: fmt::Display,
    BIn: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<BIn>) -> Self::Future {
        let rx = self.info.clone();
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);

        let span = tracing::info_span!("Connection");
        polled_span(&span);

        let fut = async move {
            async {
                tracing::trace!("getting Connection information (sent from the acceptor)");
                if let Ok(info) = rx.recv().await {
                    req.extensions_mut().insert(info);
                }
            }
            .instrument(span.clone())
            .await;
            inner.call(req).instrument(span).await
        };

        Box::pin(fut)
    }
}
