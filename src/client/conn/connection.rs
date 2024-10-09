//! Connections are responsible for sending and receiving HTTP requests and responses
//! over an arbitrary two-way stream of bytes.

use std::{fmt, future::Future, pin::Pin, task::Poll};

use crate::BoxFuture;
use http_body::Body as HttpBody;
use hyper::body::Incoming;
use thiserror::Error;

pub use crate::client::pool::key::UriError;
use crate::client::pool::PoolableConnection;

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

impl<'a, C, B> Future for WhenReady<'a, C, B>
where
    C: Connection<B> + ?Sized,
{
    type Output = Result<(), C::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        self.conn.poll_ready(cx)
    }
}

/// An HTTP connection.
pub struct HttpConnection<B> {
    inner: InnerConnection<B>,
}

impl<B> HttpConnection<B> {
    /// Create a new HTTP/1 connection.
    pub(super) fn h1(conn: hyper::client::conn::http1::SendRequest<B>) -> Self {
        HttpConnection {
            inner: InnerConnection::H1(conn),
        }
    }

    /// Create a new HTTP/2 connection.
    pub(super) fn h2(conn: hyper::client::conn::http2::SendRequest<B>) -> Self {
        HttpConnection {
            inner: InnerConnection::H2(conn),
        }
    }
}

impl<B> fmt::Debug for HttpConnection<B>
where
    B: HttpBody + Send + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpConnection")
            .field("version", &self.version())
            .finish()
    }
}

enum InnerConnection<B> {
    H2(hyper::client::conn::http2::SendRequest<B>),
    H1(hyper::client::conn::http1::SendRequest<B>),
}

impl<B> Connection<B> for HttpConnection<B>
where
    B: HttpBody + Send + 'static,
{
    type ResBody = hyper::body::Incoming;

    type Error = hyper::Error;

    type Future = BoxFuture<'static, Result<http::Response<Incoming>, hyper::Error>>;

    fn send_request(&mut self, mut request: http::Request<B>) -> Self::Future {
        match &mut self.inner {
            InnerConnection::H2(conn) => {
                *request.version_mut() = http::Version::HTTP_2;
                Box::pin(conn.send_request(request))
            }
            InnerConnection::H1(conn) => {
                *request.version_mut() = http::Version::HTTP_11;
                Box::pin(conn.send_request(request))
            }
        }
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        match &mut self.inner {
            InnerConnection::H2(conn) => conn.poll_ready(cx),
            InnerConnection::H1(conn) => conn.poll_ready(cx),
        }
    }

    fn version(&self) -> http::Version {
        match &self.inner {
            InnerConnection::H2(_) => http::Version::HTTP_2,
            InnerConnection::H1(_) => http::Version::HTTP_11,
        }
    }
}

impl<B> PoolableConnection for HttpConnection<B>
where
    B: Send + 'static,
{
    /// Checks for the connection being open by checking if the underlying connection is ready
    /// to send a new request. If the connection is not ready, it can't be re-used,
    /// so this shortcut isn't harmful.
    fn is_open(&self) -> bool {
        match &self.inner {
            InnerConnection::H2(ref conn) => conn.is_ready(),
            InnerConnection::H1(ref conn) => conn.is_ready(),
        }
    }

    /// HTTP/2 connections can be shared, but HTTP/1 connections cannot.
    fn can_share(&self) -> bool {
        match &self.inner {
            InnerConnection::H2(_) => true,
            InnerConnection::H1(_) => false,
        }
    }

    /// Reuse the connection if it is an HTTP/2 connection.
    fn reuse(&mut self) -> Option<Self> {
        match &self.inner {
            InnerConnection::H2(conn) => Some(Self {
                inner: InnerConnection::H2(conn.clone()),
            }),
            InnerConnection::H1(_) => None,
        }
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

#[cfg(test)]
mod tests {
    use crate::Body;
    use crate::BoxFuture;

    use super::Connection;

    static_assertions::assert_obj_safe!(Connection<Body, Future=BoxFuture<'static, ()>, Error=std::io::Error, ResBody=Body>);
}
