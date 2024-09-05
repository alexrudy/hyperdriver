use std::future::Future;
use std::task::{Context, Poll};

use http::{Request, Response};
use http_body::Body as HttpBody;

use crate::BoxError;

/// An asynchronous function from `Request` to `Response`.
pub trait HttpService<ReqBody> {
    /// The `HttpBody` body of the `http::Response`.
    type ResBody: HttpBody;

    /// The error type that can occur within this `Service`.
    ///
    /// Note: Returning an `Error` to a hyper server will cause the connection
    /// to be abruptly aborted. In most cases, it is better to return a `Response`
    /// with a 4xx or 5xx status code.
    type Error: Into<BoxError>;

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
    T::Error: Into<BoxError>,
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

#[cfg(feature = "client")]
pub(super) mod http1 {

    use std::fmt;
    use std::task::{Context, Poll};

    use ::http;
    use http::uri::Scheme;
    use http::Uri;

    use crate::client::conn::Connection;
    use crate::client::pool::PoolableConnection;
    use crate::client::Error;
    use crate::service::client::ExecuteRequest;
    use crate::service::error::MaybeErrorFuture;
    use crate::service::error::PreprocessService;

    type PreprocessFn<C, B, E> = fn(ExecuteRequest<C, B>) -> Result<ExecuteRequest<C, B>, E>;

    /// A service that checks if the request is HTTP/1.1 compatible.
    #[derive(Debug)]
    pub struct Http1ChecksService<S, C, B>
    where
        S: tower::Service<ExecuteRequest<C, B>, Error = Error>,
        C: Connection<B> + PoolableConnection,
    {
        inner: PreprocessService<S, PreprocessFn<C, B, S::Error>>,
    }

    impl<S, C, B> tower::Service<ExecuteRequest<C, B>> for Http1ChecksService<S, C, B>
    where
        S: tower::Service<ExecuteRequest<C, B>, Error = Error>,
        C: Connection<B> + PoolableConnection,
    {
        type Response = S::Response;

        type Error = S::Error;

        type Future = MaybeErrorFuture<S::Future, S::Response, S::Error>;

        fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.inner.poll_ready(cx)
        }

        fn call(&mut self, req: ExecuteRequest<C, B>) -> Self::Future {
            self.inner.call(req)
        }
    }

    impl<S, C, B> Clone for Http1ChecksService<S, C, B>
    where
        S: tower::Service<ExecuteRequest<C, B>, Error = Error> + Clone,
        C: Connection<B> + PoolableConnection,
    {
        fn clone(&self) -> Self {
            Self {
                inner: self.inner.clone(),
            }
        }
    }

    impl<S, C, B> Http1ChecksService<S, C, B>
    where
        S: tower::Service<ExecuteRequest<C, B>, Error = Error>,
        C: Connection<B> + PoolableConnection,
    {
        /// Create a new `Http1ChecksService`.
        pub fn new(service: S) -> Self {
            Self {
                inner: PreprocessService::new(service, check_http1_request),
            }
        }
    }

    /// A layer that checks if the request is HTTP/1.1 compatible.
    pub struct Http1ChecksLayer<C, B> {
        processor: std::marker::PhantomData<fn(C, B)>,
    }

    impl<C, B> Http1ChecksLayer<C, B> {
        /// Create a new `Http1ChecksLayer`.
        pub fn new() -> Self {
            Self {
                processor: std::marker::PhantomData,
            }
        }
    }

    impl<C, B> Default for Http1ChecksLayer<C, B> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<C, B> Clone for Http1ChecksLayer<C, B> {
        fn clone(&self) -> Self {
            Self::new()
        }
    }

    impl<C, B> fmt::Debug for Http1ChecksLayer<C, B> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("Http1ChecksLayer").finish()
        }
    }

    impl<C, B, S> tower::layer::Layer<S> for Http1ChecksLayer<C, B>
    where
        S: tower::Service<ExecuteRequest<C, B>, Error = Error>,
        C: Connection<B> + PoolableConnection,
    {
        type Service = Http1ChecksService<S, C, B>;

        fn layer(&self, service: S) -> Self::Service {
            Http1ChecksService::new(service)
        }
    }

    fn check_http1_request<C, B>(
        mut req: ExecuteRequest<C, B>,
    ) -> Result<ExecuteRequest<C, B>, Error>
    where
        C: Connection<B> + PoolableConnection,
    {
        if req.connection().version() >= http::Version::HTTP_2 {
            return Ok(req);
        }

        if req.request().method() == http::Method::CONNECT {
            authority_form(req.request_mut().uri_mut());

            // If the URI is to HTTPS, and the connector claimed to be a proxy,
            // then it *should* have tunneled, and so we don't want to send
            // absolute-form in that case.
            if req.request().uri().scheme() == Some(&Scheme::HTTPS) {
                origin_form(req.request_mut().uri_mut());
            }
        } else if req.request().uri().scheme().is_none()
            || req.request().uri().authority().is_none()
        {
            absolute_form(req.request_mut().uri_mut());
        } else {
            origin_form(req.request_mut().uri_mut());
        }

        Ok(req)
    }

    /// Convert the URI to authority-form, if it is not already.
    ///
    /// This is the form of the URI with just the authority and a default
    /// path and scheme. This is used in HTTP/1 CONNECT requests.
    fn authority_form(uri: &mut Uri) {
        *uri = match uri.authority() {
            Some(auth) => {
                let mut parts = ::http::uri::Parts::default();
                parts.authority = Some(auth.clone());
                Uri::from_parts(parts).expect("authority is valid")
            }
            None => {
                unreachable!("authority_form with relative uri");
            }
        };
    }

    fn absolute_form(uri: &mut Uri) {
        debug_assert!(uri.scheme().is_some(), "absolute_form needs a scheme");
        debug_assert!(
            uri.authority().is_some(),
            "absolute_form needs an authority"
        );
    }

    /// Convert the URI to origin-form, if it is not already.
    ///
    /// This form of the URI has no scheme or authority, and contains just
    /// the path, usually used in HTTP/1 requests.
    fn origin_form(uri: &mut Uri) {
        let path = match uri.path_and_query() {
            Some(path) if path.as_str() != "/" => {
                let mut parts = ::http::uri::Parts::default();
                parts.path_and_query = Some(path.clone());
                Uri::from_parts(parts).expect("path is valid uri")
            }
            _none_or_just_slash => {
                debug_assert!(Uri::default() == "/");
                Uri::default()
            }
        };
        *uri = path
    }

    #[cfg(test)]
    mod tests {

        use super::*;

        #[test]
        fn test_origin_form() {
            let mut uri = "http://example.com".parse().unwrap();
            origin_form(&mut uri);
            assert_eq!(uri, "/");

            let mut uri = "/some/path/here".parse().unwrap();
            origin_form(&mut uri);
            assert_eq!(uri, "/some/path/here");

            let mut uri = "http://example.com:8080/some/path?query#fragment"
                .parse()
                .unwrap();
            origin_form(&mut uri);
            assert_eq!(uri, "/some/path?query");

            let mut uri = "/".parse().unwrap();
            origin_form(&mut uri);
            assert_eq!(uri, "/");
        }

        #[test]
        fn test_absolute_form() {
            let mut uri = "http://example.com".parse().unwrap();
            absolute_form(&mut uri);
            assert_eq!(uri, "http://example.com");

            let mut uri = "http://example.com:8080".parse().unwrap();
            absolute_form(&mut uri);
            assert_eq!(uri, "http://example.com:8080");

            let mut uri = "https://example.com/some/path?query".parse().unwrap();
            absolute_form(&mut uri);
            assert_eq!(uri, "https://example.com/some/path?query");

            let mut uri = "https://example.com:8443".parse().unwrap();
            absolute_form(&mut uri);
            assert_eq!(uri, "https://example.com:8443");

            let mut uri = "http://example.com:443".parse().unwrap();
            absolute_form(&mut uri);
            assert_eq!(uri, "http://example.com:443");

            let mut uri = "https://example.com:80".parse().unwrap();
            absolute_form(&mut uri);
            assert_eq!(uri, "https://example.com:80");
        }
    }
}

#[cfg(feature = "client")]
pub(super) mod http2 {
    use std::fmt;
    use std::task::{Context, Poll};

    use ::http;

    use crate::client::conn::Connection;
    use crate::client::pool::PoolableConnection;
    use crate::client::Error;
    use crate::service::client::ExecuteRequest;
    use crate::service::error::{MaybeErrorFuture, PreprocessService};

    const CONNECTION_HEADERS: [http::HeaderName; 5] = [
        http::header::CONNECTION,
        http::HeaderName::from_static("proxy-connection"),
        http::HeaderName::from_static("keep-alive"),
        http::header::TRANSFER_ENCODING,
        http::header::UPGRADE,
    ];

    type PreprocessFn<C, B, E> = fn(ExecuteRequest<C, B>) -> Result<ExecuteRequest<C, B>, E>;

    /// A service that checks if the request is HTTP/2 compatible.
    #[derive(Debug)]
    pub struct Http2ChecksService<S, C, B>
    where
        S: tower::Service<ExecuteRequest<C, B>, Error = Error>,
        C: Connection<B> + PoolableConnection,
    {
        inner: PreprocessService<S, PreprocessFn<C, B, S::Error>>,
    }

    impl<S, C, B> Clone for Http2ChecksService<S, C, B>
    where
        S: tower::Service<ExecuteRequest<C, B>, Error = Error> + Clone,
        C: Connection<B> + PoolableConnection,
    {
        fn clone(&self) -> Self {
            Self::new(self.inner.service().clone())
        }
    }

    impl<S, C, B> Http2ChecksService<S, C, B>
    where
        S: tower::Service<ExecuteRequest<C, B>, Error = Error>,
        C: Connection<B> + PoolableConnection,
    {
        /// Create a new `Http2ChecksService`.
        pub fn new(inner: S) -> Self {
            Self {
                inner: PreprocessService::new(inner, check_http2_request),
            }
        }
    }

    fn check_http2_request<C, B>(
        mut req: ExecuteRequest<C, B>,
    ) -> Result<ExecuteRequest<C, B>, Error>
    where
        C: Connection<B> + PoolableConnection,
    {
        if req.connection().version() == http::Version::HTTP_2 {
            if req.request().method() == http::Method::CONNECT {
                return Err(Error::InvalidMethod(http::Method::CONNECT));
            }

            *req.request_mut().version_mut() = http::Version::HTTP_2;

            for connection_header in &CONNECTION_HEADERS {
                if req
                    .request_mut()
                    .headers_mut()
                    .remove(connection_header)
                    .is_some()
                {
                    tracing::warn!(
                        "removed illegal connection header {:?} from HTTP/2 request",
                        connection_header
                    );
                };
            }

            if req
                .request_mut()
                .headers_mut()
                .remove(http::header::HOST)
                .is_some()
            {
                tracing::warn!("removed illegal header `host` from HTTP/2 request");
            }
        }
        Ok(req)
    }

    impl<S, C, B> tower::Service<ExecuteRequest<C, B>> for Http2ChecksService<S, C, B>
    where
        S: tower::Service<ExecuteRequest<C, B>, Error = Error>,
        C: Connection<B> + PoolableConnection,
    {
        type Response = S::Response;

        type Error = S::Error;

        type Future = MaybeErrorFuture<S::Future, S::Response, S::Error>;

        #[inline]
        fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.inner.poll_ready(cx)
        }

        #[inline]
        fn call(&mut self, req: ExecuteRequest<C, B>) -> Self::Future {
            self.inner.call(req)
        }
    }

    /// A `Layer` that applies HTTP/2 checks to requests.
    pub struct Http2ChecksLayer<C, B> {
        _marker: std::marker::PhantomData<fn(C, B)>,
    }

    impl<C, B> Http2ChecksLayer<C, B> {
        /// Create a new `Http2ChecksLayer`.
        pub fn new() -> Self {
            Self {
                _marker: std::marker::PhantomData,
            }
        }
    }

    impl<C, B> Default for Http2ChecksLayer<C, B> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<C, B> fmt::Debug for Http2ChecksLayer<C, B> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("Http2ChecksLayer").finish()
        }
    }

    impl<C, B> Clone for Http2ChecksLayer<C, B> {
        fn clone(&self) -> Self {
            Self::new()
        }
    }

    impl<S, C, B> tower::layer::Layer<S> for Http2ChecksLayer<C, B>
    where
        S: tower::Service<ExecuteRequest<C, B>, Error = Error>,
        C: Connection<B> + PoolableConnection,
    {
        type Service = Http2ChecksService<S, C, B>;

        fn layer(&self, inner: S) -> Self::Service {
            Http2ChecksService::new(inner)
        }
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
