use std::fmt;

use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use tower::Layer;
use tower::Service;

/// Layer to convert a service which accepts a custom body type to a service
/// which accepts a [`Body`][crate::Body] body.
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

/// Extension trait for `Service` to adapt inner body types to crate::Body.
pub trait AdaptCustomBodyExt<BIn, BOut>: Sized {
    /// Adapt a service to use custom body types internally, but still accept and return
    /// [`Body`][crate::Body] as the outer body type.
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
pub struct AdaptOuterBodyLayer<BIn, BOut, F> {
    transform: F,
    _body: std::marker::PhantomData<fn(BIn) -> BOut>,
}

impl<BIn, BOut, F: Clone> Clone for AdaptOuterBodyLayer<BIn, BOut, F> {
    fn clone(&self) -> Self {
        Self {
            transform: self.transform.clone(),
            _body: std::marker::PhantomData,
        }
    }
}

impl<BIn, BOut, F> fmt::Debug for AdaptOuterBodyLayer<BIn, BOut, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdaptOuterBodyLayer").finish()
    }
}

impl<BIn, BOut, F> AdaptOuterBodyLayer<BIn, BOut, F> {
    /// Create a new `AdaptOuterBodyLayer`.
    pub fn new(transform: F) -> Self {
        Self {
            transform,
            _body: std::marker::PhantomData,
        }
    }
}

impl<BIn, BOut, F, S> Layer<S> for AdaptOuterBodyLayer<BIn, BOut, F>
where
    S: Service<http::Request<crate::Body>>,
    F: Clone,
{
    type Service = AdaptOuterBodyService<BIn, BOut, F, S>;

    fn layer(&self, inner: S) -> Self::Service {
        AdaptOuterBodyService {
            inner,
            transform: self.transform.clone(),
            _body: std::marker::PhantomData,
        }
    }
}

/// Service to accept and return a custom body type, but internally use `crate::Body`.
pub struct AdaptOuterBodyService<BIn, BOut, F, S> {
    inner: S,
    transform: F,
    _body: std::marker::PhantomData<fn(BIn) -> BOut>,
}

impl<BIn, BOut, F, S: fmt::Debug> fmt::Debug for AdaptOuterBodyService<BIn, BOut, F, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdaptOuterBodyService")
            .field("inner", &self.inner)
            .finish()
    }
}

impl<BIn, BOut, F: Clone, S: Clone> Clone for AdaptOuterBodyService<BIn, BOut, F, S> {
    fn clone(&self) -> Self {
        Self::new(self.inner.clone(), self.transform.clone())
    }
}

impl<BIn, BOut, F, S> AdaptOuterBodyService<BIn, BOut, F, S> {
    /// Create a new `AdaptOuterBodyService`.
    pub fn new(inner: S, transform: F) -> Self {
        Self {
            inner,
            transform,
            _body: std::marker::PhantomData,
        }
    }
}

impl<BIn, BOut, F, S> Service<http::Request<BIn>> for AdaptOuterBodyService<BIn, BOut, F, S>
where
    S: Service<http::Request<crate::Body>, Response = http::Response<crate::Body>>,
    BIn: Into<crate::Body>,
    F: Fn(crate::Body) -> BOut + Clone,
{
    type Response = http::Response<BOut>;
    type Error = S::Error;
    type Future = fut::AdaptOuterBodyFuture<S::Future, F, BOut, S::Error>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<BIn>) -> Self::Future {
        fut::AdaptOuterBodyFuture::new(self.inner.call(req.map(Into::into)), self.transform.clone())
    }
}

mod fut {
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
    pub struct AdaptOuterBodyFuture<Fut, F, Body, Error> {
        #[pin]
        inner: Fut,
        transform: F,
        _body: PhantomServiceFuture<Body, Error>,
    }

    impl<Fut, F, BodyOut, Error> AdaptOuterBodyFuture<Fut, F, BodyOut, Error> {
        pub(super) fn new(inner: Fut, transform: F) -> Self {
            Self {
                inner,
                transform,
                _body: std::marker::PhantomData,
            }
        }
    }

    impl<Fut, F, BodyOut, Error> Future for AdaptOuterBodyFuture<Fut, F, BodyOut, Error>
    where
        Fut: Future<Output = Result<http::Response<crate::body::Body>, Error>>,
        F: Fn(crate::Body) -> BodyOut,
    {
        type Output = Result<http::Response<BodyOut>, Error>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.project();
            match this.inner.poll(cx) {
                Poll::Ready(Ok(res)) => Poll::Ready(Ok(res.map(|body| (this.transform)(body)))),
                Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
                Poll::Pending => Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use std::convert::Infallible;

    use super::AdaptCustomBodyExt as _;
    use super::*;
    use http::Request;
    use http_body_util::Empty;
    use tower::{service_fn, BoxError};

    type EmptyBody = Empty<Bytes>;

    #[allow(dead_code)]
    fn compile_adapt_internal() -> AdaptCustomBodyService<
        EmptyBody,
        EmptyBody,
        impl Service<http::Request<EmptyBody>, Response = http::Response<EmptyBody>>,
    > {
        service_fn(|_: Request<EmptyBody>| async {
            Ok::<_, Infallible>(http::Response::new(EmptyBody::new()))
        })
        .adapt_custom_body()
    }

    #[allow(dead_code)]
    fn compile_adapt_external() -> impl Service<
        http::Request<UnsyncBoxBody<Bytes, BoxError>>,
        Response = http::Response<UnsyncBoxBody<Bytes, BoxError>>,
    > {
        AdaptOuterBodyService::new(
            service_fn(|req: http::Request<crate::Body>| async move {
                Ok::<_, BoxError>(http::Response::new(req.into_body()))
            }),
            UnsyncBoxBody::new,
        )
    }

    #[cfg(feature = "axum")]
    #[tokio::test]
    async fn adapt_external() {
        use std::convert::Infallible;

        use axum::Router;
        use http_body_util::BodyExt as _;
        use tower::ServiceExt;

        let service = service_fn(|req: http::Request<crate::Body>| async move {
            let body = req.into_body();
            Ok::<_, Infallible>(http::Response::new(body))
        });

        let app = Router::new().route_service(
            "/",
            AdaptOuterBodyService::new(service, axum::body::Body::new),
        );

        let svc = app.into_service();

        let req = http::Request::get("/")
            .body(http_body_util::Full::new(Bytes::from("hello!")))
            .unwrap();

        let res = svc.oneshot(req).await.unwrap();
        assert_eq!(res.status(), http::StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body, Bytes::from("hello!"));
    }
}
