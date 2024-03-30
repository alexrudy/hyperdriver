use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use tower::{util::Oneshot, ServiceExt as _};

#[derive(Debug, Clone)]
pub struct TowerHyperService<S> {
    service: S,
}

impl<S> TowerHyperService<S> {
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

#[pin_project::pin_project]
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
