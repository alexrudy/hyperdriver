//! Adaptors to create services which either internally or externally use hyperdriver's Body type

use std::{fmt, marker::PhantomData};

use bytes::Bytes;
use http_body_util::BodyExt as _;

use super::Body;
use crate::BoxError;

type BoxBody = http_body_util::combinators::UnsyncBoxBody<Bytes, BoxError>;

/// A service which maps an inner service's Body type into [`Body`].
#[derive(Debug, Clone)]
pub struct WrapResponseIntoBody<S> {
    inner: S,
}

impl<S> WrapResponseIntoBody<S> {
    /// Create a new Body adaptor service
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S, BOut, Req> tower::Service<Req> for WrapResponseIntoBody<S>
where
    S: tower::Service<Req, Response = http::Response<BOut>>,
    BOut: http_body::Body<Data = Bytes> + Send + 'static,
    BOut::Error: Into<BoxError>,
{
    type Response = http::Response<Body>;
    type Error = S::Error;
    type Future = self::future::WrapResponseBodyFuture<S::Future, S::Error, BOut, Body>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let future = self.inner.call(req);
        self::future::WrapResponseBodyFuture::new(future)
    }
}

/// A wrapper which allows a service to accept [`Body`] and converts
/// it to the inner service's body type.
pub struct WrapRequestFromBody<S, BIn> {
    service: S,
    body: PhantomData<fn(BIn)>,
}

impl<S, BIn> WrapRequestFromBody<S, BIn> {
    /// Create a new wrapper service
    pub fn new(service: S) -> Self {
        Self {
            service,
            body: PhantomData,
        }
    }

    /// Access a reference to the inner service
    pub fn service(&self) -> &S {
        &self.service
    }

    /// Access an exclusive reference to the inner service
    pub fn service_mut(&mut self) -> &mut S {
        &mut self.service
    }

    /// Unwrap the inner service
    pub fn into_service(self) -> S {
        self.service
    }
}

impl<S: Clone, BIn> Clone for WrapRequestFromBody<S, BIn> {
    fn clone(&self) -> Self {
        Self {
            service: self.service.clone(),
            body: PhantomData,
        }
    }
}

impl<S: fmt::Debug, BIn> fmt::Debug for WrapRequestFromBody<S, BIn> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WrapRequestFromBody")
            .field("service", &self.service)
            .finish()
    }
}

impl<S, BIn> tower::Service<http::Request<Body>> for WrapRequestFromBody<S, BIn>
where
    S: tower::Service<http::Request<BIn>>,
    BIn: From<Body> + Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<Body>) -> Self::Future {
        self.service.call(req.map(Into::into))
    }
}

/// Wrap an inner service so that it accepts and response with some Body.
pub struct WrapAsBodyService<S, BIn, B = Body> {
    service: S,
    body: PhantomData<fn(BIn) -> B>,
}

impl<S, BIn, B> WrapAsBodyService<S, BIn, B> {
    /// Create a new wrapper service
    pub fn new(service: S) -> Self {
        Self {
            service,
            body: PhantomData,
        }
    }

    /// Access a reference to the inner service
    pub fn service(&self) -> &S {
        &self.service
    }

    /// Access an exclusive reference to the inner service
    pub fn service_mut(&mut self) -> &mut S {
        &mut self.service
    }

    /// Unwrap the inner service
    pub fn into_service(self) -> S {
        self.service
    }
}

impl<S: Clone, BIn, B> Clone for WrapAsBodyService<S, BIn, B> {
    fn clone(&self) -> Self {
        Self {
            service: self.service.clone(),
            body: PhantomData,
        }
    }
}

impl<S: fmt::Debug, BIn, B> fmt::Debug for WrapAsBodyService<S, BIn, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WrapAsBodyService")
            .field("service", &self.service)
            .finish()
    }
}

impl<S, BIn, BOut, B> tower::Service<http::Request<B>> for WrapAsBodyService<S, BIn, B>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>>,
    BIn: From<BoxBody> + Send + 'static,
    BOut: http_body::Body<Data = Bytes> + Send + 'static,
    BOut::Error: Into<BoxError>,
    B: From<BoxBody> + http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<BoxError> + From<BoxError> + Send + 'static,
{
    type Response = http::Response<B>;
    type Error = S::Error;
    type Future = self::future::WrapResponseBodyFuture<S::Future, S::Error, BOut, B>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        let future = self
            .service
            .call(req.map(|body: B| BIn::from(body.map_err(Into::into).boxed_unsync())));
        self::future::WrapResponseBodyFuture::new(future)
    }
}

/// Wrap some inner service
pub struct ProvideAsBodyService<S, BRequest, BResponse> {
    service: S,
    body: PhantomData<fn(BRequest) -> BResponse>,
}

impl<S: fmt::Debug, BReq, BRes> fmt::Debug for ProvideAsBodyService<S, BReq, BRes> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProvideAsBodyService")
            .field("service", &self.service)
            .finish()
    }
}

impl<S: Clone, BReq, BRes> Clone for ProvideAsBodyService<S, BReq, BRes>
where
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            service: self.service.clone(),
            body: PhantomData,
        }
    }
}

impl<S, BOuter, BRequest, BResponse> tower::Service<http::Request<BOuter>>
    for ProvideAsBodyService<S, BRequest, BResponse>
where
    S: tower::Service<http::Request<BRequest>, Response = http::Response<BResponse>>,
    BRequest: From<BoxBody>,
    BResponse: http_body::Body<Data = Bytes> + Send + 'static,
    BResponse::Error: Into<BoxError>,
    BOuter: From<BoxBody> + http_body::Body<Data = Bytes> + Send + 'static,
    BOuter::Error: Into<BoxError>,
{
    type Response = http::Response<BOuter>;
    type Error = S::Error;
    type Future = self::future::WrapResponseBodyFuture<S::Future, S::Error, BResponse, BOuter>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<BOuter>) -> Self::Future {
        let future = self
            .service
            .call(req.map(|body: BOuter| BRequest::from(body.map_err(Into::into).boxed_unsync())));
        self::future::WrapResponseBodyFuture::new(future)
    }
}

/// Wrap a service with a pair of mapper functions to convert request and response bodies.
#[derive(Debug, Clone)]
pub struct ProvideViaMapBody<S, FReq, FRes> {
    service: S,
    map_request: FReq,
    map_respone: FRes,
}

impl<S, FReq, FRes> ProvideViaMapBody<S, FReq, FRes> {
    /// Create a new instance of `ProvideViaMapBody`.
    pub fn new(service: S, map_request: FReq, map_respone: FRes) -> Self {
        Self {
            service,
            map_request,
            map_respone,
        }
    }
}

impl<S, FReq, BReqInner, BReqOuter, BResInner, BResOuter, FRes>
    tower::Service<http::Request<BReqOuter>> for ProvideViaMapBody<S, FReq, FRes>
where
    S: tower::Service<http::Request<BReqInner>, Response = http::Response<BResInner>>,
    FReq: Fn(BReqOuter) -> BReqInner + Clone,
    FRes: Fn(BResInner) -> BResOuter + Clone,
{
    type Response = http::Response<BResOuter>;

    type Error = S::Error;

    type Future =
        self::future::MapResponseBodyFuture<S::Future, FRes, BResInner, BResOuter, S::Error>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<BReqOuter>) -> Self::Future {
        self::future::MapResponseBodyFuture::new(
            self.service.call(req.map(self.map_request.clone())),
            self.map_respone.clone(),
        )
    }
}

mod future {
    use std::fmt;
    use std::future::Future;
    use std::marker::PhantomData;
    use std::pin::Pin;
    use std::task::{ready, Context, Poll};

    use bytes::Bytes;
    use http::Response;
    use http_body_util::BodyExt as _;
    use pin_project::pin_project;

    use crate::{Body, BoxError};

    use super::BoxBody;

    #[pin_project]
    pub struct MapResponseBodyFuture<Fut, F, BIn, BOut, E> {
        #[pin]
        future: Fut,
        map_response: F,
        future_data: PhantomData<fn(BIn) -> (BOut, E)>,
    }

    impl<Fut, F, BIn, BOut, E> fmt::Debug for MapResponseBodyFuture<Fut, F, BIn, BOut, E> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("MapResponseBodyFuture").finish()
        }
    }

    impl<Fut, F, BIn, BOut, E> MapResponseBodyFuture<Fut, F, BIn, BOut, E> {
        pub(super) fn new(future: Fut, map_response: F) -> Self {
            Self {
                future,
                map_response,
                future_data: PhantomData,
            }
        }
    }

    impl<Fut, F, BIn, BOut, E> Future for MapResponseBodyFuture<Fut, F, BIn, BOut, E>
    where
        F: Fn(BIn) -> BOut,
        Fut: Future<Output = Result<http::Response<BIn>, E>>,
    {
        type Output = Result<http::Response<BOut>, E>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.project();
            let result = ready!(this.future.poll(cx));
            Poll::Ready(result.map(|response| response.map(this.map_response)))
        }
    }

    /// Future which maps a returned body type into [`Body`]
    #[pin_project]
    pub struct WrapResponseBodyFuture<F, E, BInner, BOuter = Body> {
        #[pin]
        future: F,
        #[allow(clippy::type_complexity)]
        output: PhantomData<fn() -> (BInner, BOuter, E)>,
    }

    impl<F, E, BInner, BOuter> fmt::Debug for WrapResponseBodyFuture<F, E, BInner, BOuter> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("AdaptResponseIntoBodyFuture").finish()
        }
    }

    impl<F, E, BInner, BOuter> WrapResponseBodyFuture<F, E, BInner, BOuter> {
        pub(super) fn new(future: F) -> Self {
            Self {
                future,
                output: PhantomData,
            }
        }
    }

    impl<F, E, BInner, BOuter> Future for WrapResponseBodyFuture<F, E, BInner, BOuter>
    where
        F: Future<Output = Result<Response<BInner>, E>>,
        BInner: http_body::Body<Data = Bytes> + Send + 'static,
        BInner::Error: Into<BoxError>,
        BOuter: From<BoxBody>,
    {
        type Output = Result<Response<BOuter>, E>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let result = ready!(self.project().future.poll(cx));
            Poll::Ready(result.map(|response| {
                response.map(|body| BOuter::from(body.map_err(Into::into).boxed_unsync()))
            }))
        }
    }
}

#[cfg(all(test, feature = "mocks"))]
mod test {
    use std::{marker::PhantomData, task::Poll};

    use static_assertions::assert_impl_one;

    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("mock error")]
    #[allow(dead_code)]
    struct MockError;

    #[allow(dead_code)]
    struct MockService<BIn, BOut> {
        body: PhantomData<fn(BIn) -> BOut>,
    }

    impl<BIn, BOut> tower::Service<http::Request<BIn>> for MockService<BIn, BOut> {
        type Response = http::Response<BOut>;
        type Error = ();
        type Future = std::future::Ready<Result<http::Response<BOut>, ()>>;

        fn poll_ready(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: http::Request<BIn>) -> Self::Future {
            unimplemented!()
        }
    }

    assert_impl_one!(WrapResponseIntoBody<MockService<(), BoxBody>>: tower::Service<http::Request<()>, Response=http::Response<Body>>);
    assert_impl_one!(ProvideAsBodyService<MockService<Body, hyper::body::Incoming>, Body, hyper::body::Incoming>: tower::Service<http::Request<BoxBody>, Response=http::Response<BoxBody>>);
}
