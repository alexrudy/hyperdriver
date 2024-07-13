use tower::Layer;
use tower::Service;

use crate::{Body, Request, Response};

/// Layer to convert a body to use `Body` as the request body from `hyper::body::Incoming`.
#[derive(Debug, Clone, Default)]
pub struct AdaptIncomingLayer;

impl AdaptIncomingLayer {
    /// Create a new `AdaptBodyLayer`.
    pub fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for AdaptIncomingLayer {
    type Service = AdaptIncomingService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AdaptIncomingService { inner }
    }
}

/// Adapt a service to use `Body` as the request body.
///
/// This is useful when you want to use `Body` as the request body type for a
/// service, and the outer functions require a service that accepts a body
/// type of `http::Request<hyper::body::Incoming>`.
#[derive(Debug, Clone, Default)]
pub struct AdaptIncomingService<S> {
    inner: S,
}

impl<S> AdaptIncomingService<S> {
    /// Create a new `AdaptBody` to wrap a service.
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

#[cfg(feature = "incoming")]
impl<T> Service<http::Request<hyper::body::Incoming>> for AdaptIncomingService<T>
where
    T: Service<Request, Response = Response>,
{
    type Response = Response;
    type Error = T::Error;
    type Future = T::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<hyper::body::Incoming>) -> Self::Future {
        self.inner.call(req.map(Body::from))
    }
}

#[cfg(test)]
#[cfg(feature = "incoming")]
mod tests {
    use std::convert::Infallible;

    use super::*;
    use crate::Body;
    use http::Request;
    use http_body::Body as HttpBody;

    #[allow(dead_code, clippy::async_yields_async)]
    fn compile_adapt_incoming() {
        let _ = tower::ServiceBuilder::new()
            .layer(AdaptIncomingLayer::new())
            .service(tower::service_fn(|req: Request<Body>| async move {
                assert_eq!(req.body().size_hint().exact(), Some(0));
                async { Ok::<_, Infallible>(Response::new(Body::empty())) }
            }));
    }
}
