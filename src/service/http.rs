use std::error::Error as StdError;
use std::future::Future;
use std::task::{Context, Poll};

use http::{Request, Response};
use http_body::Body as HttpBody;

/// An asynchronous function from `Request` to `Response`.
pub trait HttpService<ReqBody> {
    /// The `HttpBody` body of the `http::Response`.
    type ResBody: HttpBody;

    /// The error type that can occur within this `Service`.
    ///
    /// Note: Returning an `Error` to a hyper server will cause the connection
    /// to be abruptly aborted. In most cases, it is better to return a `Response`
    /// with a 4xx or 5xx status code.
    type Error: Into<Box<dyn StdError + Send + Sync>>;

    /// The `Future` returned by this `Service`.
    type Future: Future<Output = Result<Response<Self::ResBody>, Self::Error>>;

    #[doc(hidden)]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>>;

    #[doc(hidden)]
    fn call(&mut self, req: Request<ReqBody>) -> Self::Future;
}

impl<T, BIn, BOut> HttpService<BIn> for T
where
    T: tower::Service<Request<BIn>, Response = Response<BOut>>,
    BOut: HttpBody,
    T::Error: Into<Box<dyn StdError + Send + Sync>>,
{
    type ResBody = BOut;

    type Error = T::Error;
    type Future = T::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        tower::Service::poll_ready(self, cx)
    }

    fn call(&mut self, req: Request<BIn>) -> Self::Future {
        tower::Service::call(self, req)
    }
}

#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body_util::Empty;
    use std::{convert::Infallible, future::Ready};

    struct Svc;

    impl tower::Service<http::Request<Empty<Bytes>>> for Svc {
        type Response = http::Response<Empty<Bytes>>;
        type Error = Infallible;
        type Future = Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: http::Request<Empty<Bytes>>) -> Self::Future {
            assert_eq!(req.version(), http::Version::HTTP_11);
            std::future::ready(Ok(http::Response::new(Empty::new())))
        }
    }

    static_assertions::assert_impl_all!(Svc: HttpService<Empty<Bytes>, ResBody=Empty<Bytes>, Error=Infallible>);

    struct NotASvc;

    impl tower::Service<http::Request<()>> for Svc {
        type Response = http::Response<()>;
        type Error = Infallible;
        type Future = Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: http::Request<()>) -> Self::Future {
            assert_eq!(req.version(), http::Version::HTTP_11);
            std::future::ready(Ok(http::Response::new(())))
        }
    }

    static_assertions::assert_not_impl_all!(NotASvc: HttpService<(), ResBody=(), Error=Infallible>);
}
