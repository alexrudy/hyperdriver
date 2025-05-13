//! Connections are responsible for sending and receiving HTTP requests and responses
//! over an arbitrary two-way stream of bytes.
//!
//! The connection trait is implemented for [`hyper::client::conn::http1::SendRequest`] and
//! [`hyper::client::conn::http2::SendRequest`], allowing the native hyper types to be used
//! for [`Protocol`](super::Protocol).

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

use http_body::Body as HttpBody;
use thiserror::Error;

pub(super) use self::future::SendRequestFuture;
use crate::client::pool::PoolableConnection;
pub use crate::client::pool::UriError;

pub mod info;
pub mod notify;

/// A connection to a remote server which can send and recieve HTTP requests/responses.
///
/// Underneath, it may not use HTTP as the connection protocol, and it may use any appropriate
/// transport protocol to connect to the server.
pub trait Connection<B> {
    /// The body type for responses this connection
    type ResBody: http_body::Body + Send + 'static;

    /// The error type for this connection
    type Error: std::error::Error + Send + Sync + 'static;

    /// The future type returned by this service
    type Future: Future<Output = Result<::http::Response<Self::ResBody>, Self::Error>>
        + Send
        + 'static;

    /// Send a request to the remote server and return the response.
    fn send_request(&mut self, request: http::Request<B>) -> Self::Future;

    /// Poll the connection to see if it is ready to accept a new request.
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>>;

    /// What HTTP version is this connection using?
    fn version(&self) -> ::http::Version;
}

/// Extension trait for `Connection` providing additional methods.
pub trait ConnectionExt<B>: Connection<B> {
    /// Future which resolves when the connection is ready to accept a new request.
    fn when_ready(&mut self) -> WhenReady<'_, Self, B> {
        WhenReady::new(self)
    }
}

impl<T, B> ConnectionExt<B> for T where T: Connection<B> {}

/// A future which resolves when the connection is ready again
#[derive(Debug)]
pub struct WhenReady<'a, C, B>
where
    C: Connection<B> + ?Sized,
{
    conn: &'a mut C,
    _private: std::marker::PhantomData<fn(B)>,
}

impl<'a, C, B> WhenReady<'a, C, B>
where
    C: Connection<B> + ?Sized,
{
    pub(crate) fn new(conn: &'a mut C) -> Self {
        Self {
            conn,
            _private: std::marker::PhantomData,
        }
    }
}

impl<C, B> Future for WhenReady<'_, C, B>
where
    C: Connection<B> + ?Sized,
{
    type Output = Result<(), C::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        self.conn.poll_ready(cx)
    }
}

impl<B> Connection<B> for hyper::client::conn::http1::SendRequest<B>
where
    B: HttpBody + Send + 'static,
{
    type ResBody = hyper::body::Incoming;

    type Error = hyper::Error;

    type Future = SendRequestFuture;

    fn send_request(&mut self, mut request: http::Request<B>) -> Self::Future {
        *request.version_mut() = http::Version::HTTP_11;
        SendRequestFuture::new(hyper::client::conn::http1::SendRequest::send_request(
            self, request,
        ))
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        hyper::client::conn::http1::SendRequest::poll_ready(self, cx)
    }

    fn version(&self) -> http::Version {
        http::Version::HTTP_11
    }
}

impl<B> PoolableConnection<B> for hyper::client::conn::http1::SendRequest<B>
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

impl<B> Connection<B> for hyper::client::conn::http2::SendRequest<B>
where
    B: HttpBody + Send + 'static,
{
    type ResBody = hyper::body::Incoming;

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

    fn version(&self) -> http::Version {
        http::Version::HTTP_2
    }
}

impl<B> PoolableConnection<B> for hyper::client::conn::http2::SendRequest<B>
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
        Some(self.clone())
    }
}

/// Error returned when a connection could not be established.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConnectionError {
    /// Error connecting to the remote host via the transport
    #[error(transparent)]
    Connecting(Box<dyn std::error::Error + Send + Sync + 'static>),

    /// Error completing the handshake.
    #[error("handshake: {0}")]
    Handshake(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),

    /// Connection was cancelled, probably because another one was established.
    #[error("connection cancelled")]
    Canceled(#[source] hyper::Error),

    /// Connection was closed.
    #[error("connection closed")]
    Closed(#[source] hyper::Error),

    /// Connection timed out.
    #[error("connection timeout")]
    Timeout,

    /// Invalid URI for the connection
    #[error("invalid URI")]
    InvalidUri(#[from] UriError),
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

    static_assertions::assert_obj_safe!(Connection<Body, Future=BoxFuture<'static, ()>, Error=std::io::Error, ResBody=Body>);
}
