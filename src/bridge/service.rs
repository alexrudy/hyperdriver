use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use tower::{ServiceExt as _, util::Oneshot};

/// A hyper service that wraps a tower service.
#[derive(Debug, Clone)]
pub struct TowerHyperService<S> {
    service: S,
}

impl<S> TowerHyperService<S> {
    /// Create a new `TowerHyperService` around a tower::Service.
    pub fn new(inner: S) -> Self {
        Self { service: inner }
    }
}

impl<S, R> hyper::service::Service<R> for TowerHyperService<S>
where
    S: tower::Service<R> + Clone,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = TowerHyperFuture<S, R>;

    fn call(&self, req: R) -> Self::Future {
        TowerHyperFuture {
            future: self.service.clone().oneshot(req),
        }
    }
}

/// A future returned by `TowerHyperService`.
#[pin_project::pin_project]
#[derive(Debug)]
pub struct TowerHyperFuture<S, R>
where
    S: tower::Service<R>,
{
    #[pin]
    future: Oneshot<S, R>,
}

impl<S, R> Future for TowerHyperFuture<S, R>
where
    S: tower::Service<R>,
{
    type Output = Result<S::Response, S::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        self.project().future.poll(cx)
    }
}
