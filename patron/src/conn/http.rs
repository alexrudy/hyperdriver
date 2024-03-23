use braid::client::Stream;
use futures_util::future::BoxFuture;
use std::fmt;
use tracing::{instrument::Instrumented, Instrument};

use ::http::{Response, Version};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use thiserror::Error;
use tracing::trace;

use crate::pool::PoolableConnection;

use super::Transport;
use super::{Connection, HttpProtocol};

/// A connector which links a transport with HTTP connections.
///
/// This connector supports HTTP/2 and HTTP/1.1 connections.
#[derive(Debug, Clone)]
pub struct HttpConnector {
    builder: HttpConnectionBuilder,
}

impl HttpConnector {
    /// Create a new connector with the given transport.
    pub fn new(builder: HttpConnectionBuilder) -> Self {
        Self { builder }
    }
}

impl tower::Service<Transport> for HttpConnector {
    type Response = HttpConnection;

    type Error = ConnectionError;

    type Future = Instrumented<future::HttpConnectFuture>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    #[tracing::instrument("http-connect", skip(self, req), fields(host = %req.host().unwrap_or("-")))]
    fn call(&mut self, req: Transport) -> Self::Future {
        future::HttpConnectFuture::new(self.builder.clone(), req).in_current_span()
    }
}

mod future {
    use std::fmt;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use pin_project::pin_project;

    use crate::conn::Transport;

    use super::ConnectionError;
    use super::HttpConnection;
    use super::HttpConnectionBuilder;

    struct DebugLiteral<T: fmt::Display>(T);

    impl<T: fmt::Display> fmt::Debug for DebugLiteral<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    type BoxFuture<'a, T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

    #[pin_project(project = StateProj, project_replace = StateProjReplace)]
    enum State {
        Error(Option<ConnectionError>),
        Handshaking {
            future: BoxFuture<'static, HttpConnection, ConnectionError>,
        },
    }

    #[pin_project]
    pub struct HttpConnectFuture {
        #[pin]
        state: State,
    }

    impl fmt::Debug for HttpConnectFuture {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let field = match &self.state {
                State::Error(_) => "Error",
                State::Handshaking { .. } => "Handshaking",
            };

            f.debug_struct("HttpConnectFuture")
                .field("state", &DebugLiteral(field))
                .finish()
        }
    }

    impl HttpConnectFuture {
        pub(super) fn new(builder: HttpConnectionBuilder, stream: Transport) -> HttpConnectFuture {
            let future = Box::pin(async move { builder.handshake(stream).await });

            Self {
                state: State::Handshaking { future },
            }
        }

        #[allow(dead_code)]
        pub(super) fn error(err: ConnectionError) -> Self {
            Self {
                state: State::Error(Some(err)),
            }
        }
    }

    impl Future for HttpConnectFuture {
        type Output = Result<HttpConnection, ConnectionError>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let mut this = self.project();
            loop {
                match this.state.as_mut().project() {
                    StateProj::Handshaking { future } => {
                        return future.as_mut().poll(cx);
                    }
                    StateProj::Error(error) => {
                        if let Some(error) = error.take() {
                            return Poll::Ready(Err(error));
                        } else {
                            panic!("invalid future state");
                        }
                    }
                }
            }
        }
    }
}

pub struct HttpConnection {
    inner: InnerConnection,
}

impl fmt::Debug for HttpConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConnection")
            .field("version", &self.version())
            .finish()
    }
}

enum InnerConnection {
    H2(hyper::client::conn::http2::SendRequest<arnold::Body>),
    H1(hyper::client::conn::http1::SendRequest<arnold::Body>),
}

impl Connection for HttpConnection {
    type Error = hyper::Error;

    type Future = BoxFuture<'static, Result<Response<Incoming>, hyper::Error>>;

    fn send_request(&mut self, request: arnold::Request) -> Self::Future {
        match &mut self.inner {
            InnerConnection::H2(conn) => Box::pin(conn.send_request(request)),
            InnerConnection::H1(conn) => Box::pin(conn.send_request(request)),
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
            InnerConnection::H2(_) => Version::HTTP_2,
            InnerConnection::H1(_) => Version::HTTP_11,
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
    /// Error connecting to the remote host via TCP
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
}

#[derive(Debug, Clone)]
pub struct HttpConnectionBuilder {
    http1: hyper::client::conn::http1::Builder,
    http2: hyper::client::conn::http2::Builder<TokioExecutor>,
    pub(crate) protocol: HttpProtocol,
}

impl HttpConnectionBuilder {
    pub fn set_protocol(&mut self, protocol: HttpProtocol) -> &mut Self {
        self.protocol = protocol;
        self
    }

    pub fn http1(&mut self) -> &mut hyper::client::conn::http1::Builder {
        &mut self.http1
    }

    pub fn http2(&mut self) -> &mut hyper::client::conn::http2::Builder<TokioExecutor> {
        &mut self.http2
    }
}

impl HttpConnectionBuilder {
    async fn handshake_h2(&self, stream: Stream) -> Result<HttpConnection, ConnectionError> {
        trace!("handshake http2");
        let (sender, conn) = self
            .http2
            .handshake(TokioIo::new(stream))
            .await
            .map_err(|error| ConnectionError::Handshake(error.into()))?;
        tokio::spawn(async {
            if let Err(err) = conn.await {
                tracing::error!(%err, "h2 connection error");
            }
        });
        Ok(HttpConnection {
            inner: InnerConnection::H2(sender),
        })
    }

    async fn handshake_h1(&self, stream: Stream) -> Result<HttpConnection, ConnectionError> {
        trace!("handshake http1");
        let (sender, conn) = self
            .http1
            .handshake(TokioIo::new(stream))
            .await
            .map_err(|error| ConnectionError::Handshake(error.into()))?;
        tokio::spawn(async {
            if let Err(err) = conn.await {
                tracing::error!(%err, "h1 connection error");
            }
        });
        Ok(HttpConnection {
            inner: InnerConnection::H1(sender),
        })
    }

    pub(crate) async fn handshake(
        &self,
        transport: Transport,
    ) -> Result<HttpConnection, ConnectionError> {
        match self.protocol {
            HttpProtocol::Http2 => self.handshake_h2(transport.into()).await,
            HttpProtocol::Http1 => {
                if transport
                    .info()
                    .tls
                    .as_ref()
                    .and_then(|tls| tls.alpn.as_ref())
                    == Some(&braid::info::Protocol::Http(http::Version::HTTP_2))
                {
                    trace!("alpn h2 switching");
                    return self.handshake_h2(transport.into()).await;
                }

                self.handshake_h1(transport.into()).await
            }
        }
    }
}

impl Default for HttpConnectionBuilder {
    fn default() -> Self {
        Self {
            http1: hyper::client::conn::http1::Builder::new(),
            http2: hyper::client::conn::http2::Builder::new(TokioExecutor::new()),
            protocol: HttpProtocol::Http1,
        }
    }
}
