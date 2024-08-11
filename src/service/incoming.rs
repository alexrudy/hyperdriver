use std::fmt::Debug;

use tower::Layer;
use tower::Service;

/// Layer to convert a body to use `Body` as the request body from `hyper::body::Incoming`.
#[derive(Debug, Clone)]
pub struct AdaptIncomingLayer<BIn, BOut> {
    body: std::marker::PhantomData<fn(BIn) -> BOut>,
}

impl<BIn, BOut> Default for AdaptIncomingLayer<BIn, BOut> {
    fn default() -> Self {
        Self {
            body: std::marker::PhantomData,
        }
    }
}

impl<BIn, BOut> AdaptIncomingLayer<BIn, BOut> {
    /// Create a new `AdaptBodyLayer`.
    pub fn new() -> Self {
        Self {
            body: std::marker::PhantomData,
        }
    }
}

impl<BIn, BOut, S> Layer<S> for AdaptIncomingLayer<BIn, BOut> {
    type Service = AdaptIncomingService<S, BIn, BOut>;

    fn layer(&self, inner: S) -> Self::Service {
        AdaptIncomingService {
            inner,
            body: std::marker::PhantomData,
        }
    }
}

/// Adapt a service to use `Body` as the request body.
///
/// This is useful when you want to use `Body` as the request body type for a
/// service, and the outer functions require a service that accepts a body
/// type of `http::Request<hyper::body::Incoming>`.
pub struct AdaptIncomingService<S, BIn, BOut> {
    inner: S,
    body: std::marker::PhantomData<fn(BIn) -> BOut>,
}

impl<S: Debug, BIn, BOut> Debug for AdaptIncomingService<S, BIn, BOut> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AdaptIncomingService")
            .field(&self.inner)
            .finish()
    }
}

impl<S: Default, BIn, BOut> Default for AdaptIncomingService<S, BIn, BOut> {
    fn default() -> Self {
        Self {
            inner: S::default(),
            body: std::marker::PhantomData,
        }
    }
}

impl<S: Clone, BIn, BOut> Clone for AdaptIncomingService<S, BIn, BOut> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            body: std::marker::PhantomData,
        }
    }
}

impl<S, BIn, BOut> AdaptIncomingService<S, BIn, BOut> {
    /// Create a new `AdaptBody` to wrap a service.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            body: std::marker::PhantomData,
        }
    }
}

#[cfg(feature = "incoming")]
impl<T, BIn, BOut> Service<http::Request<hyper::body::Incoming>>
    for AdaptIncomingService<T, BIn, BOut>
where
    T: Service<http::Request<BIn>, Response = http::Response<BOut>>,
    BIn: From<hyper::body::Incoming>,
{
    type Response = http::Response<BOut>;
    type Error = T::Error;
    type Future = T::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<hyper::body::Incoming>) -> Self::Future {
        self.inner.call(req.map(BIn::from))
    }
}

#[cfg(test)]
#[cfg(feature = "incoming")]
mod tests {
    use std::convert::Infallible;

    use super::*;
    use crate::body::Body;
    use http_body::Body as HttpBody;

    #[allow(dead_code, clippy::async_yields_async)]
    fn compile_adapt_incoming() {
        let _ = tower::ServiceBuilder::new()
            .layer(AdaptIncomingLayer::<Body, Body>::new())
            .service(tower::service_fn(|req: http::Request<Body>| async move {
                assert_eq!(req.body().size_hint().exact(), Some(0));
                async { Ok::<_, Infallible>(http::Response::new(Body::empty())) }
            }));
    }
}
