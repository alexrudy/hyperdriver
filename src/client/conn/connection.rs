//! Connections are responsible for sending and receiving HTTP requests and responses
//! over an arbitrary two-way stream of bytes.

use std::{fmt, future::Future};

use futures_core::future::BoxFuture;
use futures_util::FutureExt as _;
use http::Response;
use hyper::body::Incoming;
use thiserror::Error;

pub use crate::client::pool::key::UriError;
use crate::client::pool::PoolableConnection;

/// A connection to a remote server which can send and recieve HTTP requests/responses.
///
/// Underneath, it may not use HTTP as the connection protocol, and it may use any appropriate
/// transport protocol to connect to the server.
pub trait Connection {
    /// The body type for responses this connection
    type ResBody: http_body::Body + Send + 'static;

    /// The error type for this connection
    type Error: std::error::Error + Send + Sync + 'static;

    /// The future type returned by this service
    type Future: Future<Output = Result<::http::Response<Self::ResBody>, Self::Error>>
        + Send
        + 'static;

    /// Send a request to the remote server and return the response.
    fn send_request(&mut self, request: crate::body::Request) -> Self::Future;

    /// Poll the connection to see if it is ready to accept a new request.
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>>;

    /// Future which resolves when the connection is ready to accept a new request.
    fn when_ready(&mut self) -> BoxFuture<'_, Result<(), Self::Error>>
    where
        Self: Send,
    {
        futures_util::future::poll_fn(|cx| self.poll_ready(cx)).boxed()
    }

    /// What HTTP version is this connection using?
    fn version(&self) -> ::http::Version;
}

/// An HTTP connection.
pub struct HttpConnection {
    inner: InnerConnection,
}

impl HttpConnection {
    /// Create a new HTTP/1 connection.
    pub(super) fn h1(conn: hyper::client::conn::http1::SendRequest<crate::body::Body>) -> Self {
        HttpConnection {
            inner: InnerConnection::H1(conn),
        }
    }

    /// Create a new HTTP/2 connection.
    pub(super) fn h2(conn: hyper::client::conn::http2::SendRequest<crate::body::Body>) -> Self {
        HttpConnection {
            inner: InnerConnection::H2(conn),
        }
    }
}

impl fmt::Debug for HttpConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpConnection")
            .field("version", &self.version())
            .finish()
    }
}

enum InnerConnection {
    H2(hyper::client::conn::http2::SendRequest<crate::body::Body>),
    H1(hyper::client::conn::http1::SendRequest<crate::body::Body>),
}

impl Connection for HttpConnection {
    type ResBody = hyper::body::Incoming;

    type Error = hyper::Error;

    type Future = BoxFuture<'static, Result<Response<Incoming>, hyper::Error>>;

    fn send_request(&mut self, mut request: crate::body::Request) -> Self::Future {
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

impl PoolableConnection for HttpConnection {
    fn is_open(&self) -> bool {
        match &self.inner {
            InnerConnection::H2(ref conn) => conn.is_ready(),
            InnerConnection::H1(ref conn) => conn.is_ready(),
        }
    }

    fn can_share(&self) -> bool {
        match &self.inner {
            InnerConnection::H2(_) => true,
            InnerConnection::H1(_) => false,
        }
    }

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
