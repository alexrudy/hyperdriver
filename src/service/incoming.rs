use std::fmt::Debug;

use tower::Layer;
use tower::Service;

/// Layer to convert a body to use `Body` as the request body from `hyper::body::Incoming`.
#[derive(Debug, Clone)]
pub struct IncomingRequestLayer<BIn, BOut> {
    body: std::marker::PhantomData<fn(BIn) -> BOut>,
}

impl<BIn, BOut> Default for IncomingRequestLayer<BIn, BOut> {
    fn default() -> Self {
        Self {
            body: std::marker::PhantomData,
        }
    }
}

impl<BIn, BOut> IncomingRequestLayer<BIn, BOut> {
    /// Create a new `AdaptBodyLayer`.
    pub fn new() -> Self {
        Self {
            body: std::marker::PhantomData,
        }
    }
}

impl<BIn, BOut, S> Layer<S> for IncomingRequestLayer<BIn, BOut> {
    type Service = IncomingRequestService<S, BIn, BOut>;

    fn layer(&self, inner: S) -> Self::Service {
        IncomingRequestService {
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
pub struct IncomingRequestService<S, BIn, BOut> {
    inner: S,
    body: std::marker::PhantomData<fn(BIn) -> BOut>,
}

impl<S: Debug, BIn, BOut> Debug for IncomingRequestService<S, BIn, BOut> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AdaptIncomingService")
            .field(&self.inner)
            .finish()
    }
}

impl<S: Default, BIn, BOut> Default for IncomingRequestService<S, BIn, BOut> {
    fn default() -> Self {
        Self {
            inner: S::default(),
            body: std::marker::PhantomData,
        }
    }
}

impl<S: Clone, BIn, BOut> Clone for IncomingRequestService<S, BIn, BOut> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            body: std::marker::PhantomData,
        }
    }
}

impl<S, BIn, BOut> IncomingRequestService<S, BIn, BOut> {
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
    for IncomingRequestService<T, BIn, BOut>
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

/// Layer to convert a service to use `Body` as the response body from `hyper::body::Incoming`.
///
/// Useful in the client context, where hyper will return `hyper::body::Incoming` as the response
/// body type, but outer services might want to use a body type which could also be constructed
/// in user space.
#[derive(Debug, Clone)]
pub struct IncomingResponseLayer<BIn, BOut> {
    body: std::marker::PhantomData<fn(BIn) -> BOut>,
}

impl<BIn, BOut> Default for IncomingResponseLayer<BIn, BOut> {
    fn default() -> Self {
        Self {
            body: std::marker::PhantomData,
        }
    }
}

impl<BIn, BOut> IncomingResponseLayer<BIn, BOut> {
    /// Create a new `AdaptBodyLayer`.
    pub fn new() -> Self {
        Self {
            body: std::marker::PhantomData,
        }
    }
}

impl<BIn, BOut, S> Layer<S> for IncomingResponseLayer<BIn, BOut> {
    type Service = IncomingResponseService<S, BIn, BOut>;

    fn layer(&self, inner: S) -> Self::Service {
        IncomingResponseService {
            inner,
            body: std::marker::PhantomData,
        }
    }
}

/// Wraps a service which returns a `http::Response<hyper::body::Incoming>` and converts it to a
/// `http::Response<BOut>`.
///
/// This is useful when you want to use `Body` as the response body type for a service in the
/// client context.
pub struct IncomingResponseService<S, BIn, BOut> {
    inner: S,
    body: std::marker::PhantomData<fn(BIn) -> BOut>,
}

impl<S: Debug, BIn, BOut> Debug for IncomingResponseService<S, BIn, BOut> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("IncomingResponseService")
            .field(&self.inner)
            .finish()
    }
}

impl<S: Default, BIn, BOut> Default for IncomingResponseService<S, BIn, BOut> {
    fn default() -> Self {
        Self {
            inner: S::default(),
            body: std::marker::PhantomData,
        }
    }
}

impl<S: Clone, BIn, BOut> Clone for IncomingResponseService<S, BIn, BOut> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            body: std::marker::PhantomData,
        }
    }
}

impl<S, BIn, BOut> IncomingResponseService<S, BIn, BOut> {
    /// Create a new `AdaptBody` to wrap a service.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            body: std::marker::PhantomData,
        }
    }
}

impl<T, BIn, BOut> Service<http::Request<BIn>> for IncomingResponseService<T, BIn, BOut>
where
    T: Service<http::Request<BIn>, Response = http::Response<hyper::body::Incoming>>,
    BOut: From<hyper::body::Incoming>,
{
    type Response = http::Response<BOut>;
    type Error = T::Error;
    type Future = future::IncomingResponseFuture<T::Future, BOut, T::Error>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<BIn>) -> Self::Future {
        future::IncomingResponseFuture::new(self.inner.call(req))
    }
}

mod future {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    #[derive(Debug)]
    #[pin_project::pin_project]
    pub struct IncomingResponseFuture<F, BOut, Error> {
        #[pin]
        future: F,
        body: std::marker::PhantomData<fn() -> (BOut, Error)>,
    }

    impl<F, BOut, Error> IncomingResponseFuture<F, BOut, Error> {
        pub fn new(future: F) -> Self {
            Self {
                future,
                body: std::marker::PhantomData,
            }
        }
    }

    impl<F, BOut, Error> Future for IncomingResponseFuture<F, BOut, Error>
    where
        F: Future<Output = Result<http::Response<hyper::body::Incoming>, Error>>,
        BOut: From<hyper::body::Incoming>,
    {
        type Output = Result<http::Response<BOut>, Error>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            match self.project().future.poll(cx) {
                Poll::Ready(res) => Poll::Ready(res.map(|res| res.map(BOut::from))),
                Poll::Pending => Poll::Pending,
            }
        }
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
            .layer(IncomingRequestLayer::<Body, Body>::new())
            .service(tower::service_fn(|req: http::Request<Body>| async move {
                assert_eq!(req.body().size_hint().exact(), Some(0));
                async { Ok::<_, Infallible>(http::Response::new(Body::empty())) }
            }));
    }
}
