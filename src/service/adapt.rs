use std::fmt;

use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use tower::Layer;
use tower::Service;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Layer to convert a service to use custom body types.
pub struct AdaptCustomBodyLayer<BIn, BOut> {
    _body: std::marker::PhantomData<fn(BIn) -> BOut>,
}

impl<BIn, BOut> fmt::Debug for AdaptCustomBodyLayer<BIn, BOut> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdaptCustomBodyLayer").finish()
    }
}

impl<BIn, BOut> Clone for AdaptCustomBodyLayer<BIn, BOut> {
    fn clone(&self) -> Self {
        Self::new()
    }
}

impl<BIn, BOut> Default for AdaptCustomBodyLayer<BIn, BOut> {
    fn default() -> Self {
        Self::new()
    }
}

impl<BIn, BOut> AdaptCustomBodyLayer<BIn, BOut> {
    /// Create a new `AdaptCustomBodyLayer`.
    pub fn new() -> Self {
        Self {
            _body: std::marker::PhantomData,
        }
    }
}

impl<BIn, BOut, S> Layer<S> for AdaptCustomBodyLayer<BIn, BOut>
where
    S: Service<http::Request<BOut>>,
{
    type Service = AdaptCustomBodyService<BIn, BOut, S>;

    fn layer(&self, inner: S) -> Self::Service {
        AdaptCustomBodyService {
            inner,
            _body: std::marker::PhantomData,
        }
    }
}

/// Adapt a service to use custom body types internally,
/// while still accepting and returning `Body` as the outer body type.
///
/// This is useful when interfacing with other libraries which want to bring their
/// own body types, but you want to use `Body` as the outer body type and use `hyperdriver::Server`.
pub struct AdaptCustomBodyService<BIn, BOut, S> {
    inner: S,
    _body: std::marker::PhantomData<fn(BIn) -> BOut>,
}

impl<BIn, BOut, S: fmt::Debug> fmt::Debug for AdaptCustomBodyService<BIn, BOut, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdaptCustomBodyService")
            .field("inner", &self.inner)
            .finish()
    }
}

impl<BIn, BOut, S: Clone> Clone for AdaptCustomBodyService<BIn, BOut, S> {
    fn clone(&self) -> Self {
        Self::new(self.inner.clone())
    }
}

impl<BIn, BOut, S: Default> Default for AdaptCustomBodyService<BIn, BOut, S> {
    fn default() -> Self {
        Self::new(S::default())
    }
}

impl<BIn, BOut, S> AdaptCustomBodyService<BIn, BOut, S> {
    /// Create a new `AdaptCustomBodyService`.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            _body: std::marker::PhantomData,
        }
    }
}

impl<BIn, BOut, S> Service<http::Request<crate::Body>> for AdaptCustomBodyService<BIn, BOut, S>
where
    S: Service<http::Request<BIn>, Response = http::Response<BOut>>,
    BIn: From<UnsyncBoxBody<Bytes, Box<dyn std::error::Error + Send + Sync + 'static>>>,
    BOut: http_body::Body<Data = Bytes> + Send + 'static,
    BOut::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Response = http::Response<crate::Body>;
    type Error = S::Error;
    type Future = fut::AdaptBodyFuture<S::Future, BOut, S::Error>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<crate::Body>) -> Self::Future {
        fut::AdaptBodyFuture::new(self.inner.call(req.map(|b| b.as_boxed().into())))
    }
}

mod fut {
    use super::BoxError;
    use bytes::Bytes;
    use pin_project::pin_project;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    type PhantomServiceFuture<Body, Error> =
        std::marker::PhantomData<fn() -> Result<http::Response<Body>, Error>>;

    #[pin_project]
    #[derive(Debug)]
    pub struct AdaptBodyFuture<Fut, Body, Error> {
        #[pin]
        inner: Fut,
        _body: PhantomServiceFuture<Body, Error>,
    }

    impl<Fut, Body, Error> AdaptBodyFuture<Fut, Body, Error> {
        pub(super) fn new(inner: Fut) -> Self {
            Self {
                inner,
                _body: std::marker::PhantomData,
            }
        }
    }

    impl<Fut, Body, Error> Future for AdaptBodyFuture<Fut, Body, Error>
    where
        Fut: Future<Output = Result<http::Response<Body>, Error>>,
        Body: http_body::Body<Data = Bytes> + Send + 'static,
        Body::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        type Output = Result<http::Response<crate::body::Body>, Error>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.project();
            this.inner
                .poll(cx)
                .map(|res| res.map(|res| res.map(crate::Body::new)))
        }
    }

    #[derive(Debug)]
    #[pin_project]
    pub struct AdaptOuterBodyFuture<Fut, Body, Error> {
        #[pin]
        inner: Fut,
        _body: PhantomServiceFuture<Body, Error>,
    }

    impl<Fut, BodyOut, Error> AdaptOuterBodyFuture<Fut, BodyOut, Error> {
        pub(super) fn new(inner: Fut) -> Self {
            Self {
                inner,
                _body: std::marker::PhantomData,
            }
        }
    }

    impl<Fut, BodyOut, Error> Future for AdaptOuterBodyFuture<Fut, BodyOut, Error>
    where
        Fut: Future<Output = Result<http::Response<crate::body::Body>, Error>>,
        BodyOut: From<http_body_util::combinators::UnsyncBoxBody<Bytes, BoxError>>,
    {
        type Output = Result<http::Response<BodyOut>, Error>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.project();
            match this.inner.poll(cx) {
                Poll::Ready(Ok(res)) => Poll::Ready(Ok(res.map(|body| body.as_boxed().into()))),
                Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
                Poll::Pending => Poll::Pending,
            }
        }
    }
}

/// Extension trait for `Service` to adapt inner body types to crate::Body.
pub trait AdaptCustomBodyExt<BIn, BOut>: Sized {
    /// Adapt a service to use custom body types internally, but still accept and return
    /// `Body` as the outer body type.
    fn adapt_custom_body(self) -> AdaptCustomBodyService<BIn, BOut, Self>;
}

impl<BIn, BOut, S> AdaptCustomBodyExt<BIn, BOut> for S
where
    S: Service<http::Request<BIn>, Response = http::Response<BOut>>,
{
    fn adapt_custom_body(self) -> AdaptCustomBodyService<BIn, BOut, S> {
        AdaptCustomBodyService::new(self)
    }
}

/// Adapt a service to externally accept and return a custom body type, but internally use
/// `crate::Body`.
pub struct AdaptOuterBodyLayer<BIn, BOut> {
    _body: std::marker::PhantomData<fn(BIn) -> BOut>,
}

impl<BIn, BOut> Default for AdaptOuterBodyLayer<BIn, BOut> {
    fn default() -> Self {
        Self::new()
    }
}

impl<BIn, BOut> Clone for AdaptOuterBodyLayer<BIn, BOut> {
    fn clone(&self) -> Self {
        Self::new()
    }
}

impl<BIn, BOut> fmt::Debug for AdaptOuterBodyLayer<BIn, BOut> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdaptOuterBodyLayer").finish()
    }
}

impl<BIn, BOut> AdaptOuterBodyLayer<BIn, BOut> {
    /// Create a new `AdaptOuterBodyLayer`.
    pub fn new() -> Self {
        Self {
            _body: std::marker::PhantomData,
        }
    }
}

impl<BIn, BOut, S> Layer<S> for AdaptOuterBodyLayer<BIn, BOut>
where
    S: Service<http::Request<BIn>>,
{
    type Service = AdaptOuterBodyService<BIn, BOut, S>;

    fn layer(&self, inner: S) -> Self::Service {
        AdaptOuterBodyService {
            inner,
            _body: std::marker::PhantomData,
        }
    }
}

/// Service to accept and return a custom body type, but internally use `crate::Body`.
pub struct AdaptOuterBodyService<BIn, BOut, S> {
    inner: S,
    _body: std::marker::PhantomData<fn(BIn) -> BOut>,
}

impl<BIn, BOut, S: fmt::Debug> fmt::Debug for AdaptOuterBodyService<BIn, BOut, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdaptOuterBodyService")
            .field("inner", &self.inner)
            .finish()
    }
}

impl<BIn, BOut, S: Clone> Clone for AdaptOuterBodyService<BIn, BOut, S> {
    fn clone(&self) -> Self {
        Self::new(self.inner.clone())
    }
}

impl<BIn, BOut, S: Default> Default for AdaptOuterBodyService<BIn, BOut, S> {
    fn default() -> Self {
        Self::new(S::default())
    }
}

impl<BIn, BOut, S> AdaptOuterBodyService<BIn, BOut, S> {
    /// Create a new `AdaptOuterBodyService`.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            _body: std::marker::PhantomData,
        }
    }
}

impl<BIn, BOut, S> Service<http::Request<BIn>> for AdaptOuterBodyService<BIn, BOut, S>
where
    S: Service<http::Request<crate::Body>, Response = http::Response<crate::Body>>,
    BIn: Into<crate::Body>,
    BOut: From<http_body_util::combinators::UnsyncBoxBody<Bytes, BoxError>>,
{
    type Response = http::Response<BOut>;
    type Error = S::Error;
    type Future = fut::AdaptOuterBodyFuture<S::Future, BOut, S::Error>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<BIn>) -> Self::Future {
        fut::AdaptOuterBodyFuture::new(self.inner.call(req.map(Into::into)))
    }
}
