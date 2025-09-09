//! Connections are responsible for sending and receiving HTTP requests and responses
//! over an arbitrary two-way stream of bytes.
//!
//! The connection trait is implemented for [`hyper::client::conn::http1::SendRequest`] and
//! [`hyper::client::conn::http2::SendRequest`], allowing the native hyper types to be used
//! for [`Protocol`](super::Protocol).

use std::ops::{Deref, DerefMut};

use chateau::client::conn::Connection;
use chateau::client::pool::PoolableConnection;
use http_body::Body as HttpBody;

pub(super) use self::future::SendRequestFuture;
use crate::service::HttpConnection;

#[derive(Debug)]
pub struct Http1Connection<B>(hyper::client::conn::http1::SendRequest<B>);

impl<B> Http1Connection<B> {
    pub fn new(send_request: hyper::client::conn::http1::SendRequest<B>) -> Self {
        Self(send_request)
    }
}

impl<B> Deref for Http1Connection<B> {
    type Target = hyper::client::conn::http1::SendRequest<B>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<B> DerefMut for Http1Connection<B> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<B> Connection<http::Request<B>> for Http1Connection<B>
where
    B: HttpBody + Send + 'static,
{
    type Response = http::Response<hyper::body::Incoming>;

    type Error = hyper::Error;

    type Future = SendRequestFuture;

    fn send_request(&mut self, mut request: http::Request<B>) -> Self::Future {
        *request.version_mut() = http::Version::HTTP_11;
        SendRequestFuture::new(hyper::client::conn::http1::SendRequest::send_request(
            &mut self.0,
            request,
        ))
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        hyper::client::conn::http1::SendRequest::poll_ready(&mut self.0, cx)
    }
}

impl<B> HttpConnection<B> for Http1Connection<B>
where
    B: HttpBody + Send + 'static,
{
    fn version(&self) -> http::Version {
        http::Version::HTTP_11
    }
}

impl<B> PoolableConnection<http::Request<B>> for Http1Connection<B>
where
    B: HttpBody + Send + 'static,
{
    fn is_open(&self) -> bool {
        self.is_ready()
    }

    fn can_share(&self) -> bool {
        false
    }

    fn reuse(&mut self) -> Option<Self> {
        None
    }
}

#[derive(Debug, Clone)]
pub struct Http2Connection<B>(hyper::client::conn::http2::SendRequest<B>);

impl<B> Http2Connection<B> {
    pub fn new(send_request: hyper::client::conn::http2::SendRequest<B>) -> Self {
        Self(send_request)
    }
}

impl<B> Deref for Http2Connection<B> {
    type Target = hyper::client::conn::http2::SendRequest<B>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<B> DerefMut for Http2Connection<B> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<B> Connection<http::Request<B>> for Http2Connection<B>
where
    B: HttpBody + Send + 'static,
{
    type Response = http::Response<hyper::body::Incoming>;

    type Error = hyper::Error;

    type Future = SendRequestFuture;

    fn send_request(&mut self, request: http::Request<B>) -> Self::Future {
        SendRequestFuture::new(hyper::client::conn::http2::SendRequest::send_request(
            self, request,
        ))
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        hyper::client::conn::http2::SendRequest::poll_ready(self, cx)
    }
}

impl<B> HttpConnection<B> for Http2Connection<B>
where
    B: HttpBody + Send + 'static,
{
    fn version(&self) -> http::Version {
        http::Version::HTTP_2
    }
}

impl<B> PoolableConnection<http::Request<B>> for Http2Connection<B>
where
    B: HttpBody + Send + 'static,
{
    fn is_open(&self) -> bool {
        hyper::client::conn::http2::SendRequest::is_ready(self)
    }

    fn can_share(&self) -> bool {
        true
    }

    fn reuse(&mut self) -> Option<Self> {
        Some(Http2Connection(self.clone()))
    }
}

/// Opaque future for connections
mod future {
    use std::fmt;
    use std::future::Future;
    use std::pin::Pin;

    /// Opaque future for sending a request over a connection.
    pub struct SendRequestFuture {
        inner: Pin<
            Box<
                dyn Future<Output = Result<http::Response<hyper::body::Incoming>, hyper::Error>>
                    + Send
                    + 'static,
            >,
        >,
    }

    impl fmt::Debug for SendRequestFuture {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("SendRequestFuture").finish()
        }
    }

    impl SendRequestFuture {
        pub(in crate::client::conn) fn new<F>(future: F) -> Self
        where
            F: Future<Output = Result<http::Response<hyper::body::Incoming>, hyper::Error>>
                + Send
                + 'static,
        {
            Self {
                inner: Box::pin(future),
            }
        }
    }

    impl Future for SendRequestFuture {
        type Output = Result<http::Response<hyper::body::Incoming>, hyper::Error>;

        fn poll(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            self.inner.as_mut().poll(cx)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Body;
    use crate::BoxFuture;

    use super::Connection;

    static_assertions::assert_obj_safe!(
        Connection<
            http::Request<Body>,
            Future = BoxFuture<'static, ()>,
            Error = std::io::Error,
            Response = http::Response<Body>,
        >
    );
}
