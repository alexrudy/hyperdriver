//! HTTP connection handling.

use ::http::{Response, Version};
use braid::client::Stream;
use bridge::io::TokioIo;
use bridge::rt::TokioExecutor;
use futures_util::future::BoxFuture;
use hyper::body::Incoming;
use std::fmt;
use thiserror::Error;
use tracing::trace;

use crate::pool::PoolableConnection;

use super::TransportStream;
use super::{Connection, HttpProtocol};

/// A builder for configuring and starting HTTP connections.
#[derive(Debug, Clone)]
pub struct HttpConnectionBuilder {
    http1: hyper::client::conn::http1::Builder,
    http2: hyper::client::conn::http2::Builder<TokioExecutor>,
    pub(crate) protocol: HttpProtocol,
}

impl HttpConnectionBuilder {
    /// Set the default protocol for the connection.
    pub fn set_protocol(&mut self, protocol: HttpProtocol) -> &mut Self {
        self.protocol = protocol;
        self
    }

    /// Get the HTTP/1.1 configuration.
    pub fn http1(&mut self) -> &mut hyper::client::conn::http1::Builder {
        &mut self.http1
    }

    /// Get the HTTP/2 configuration.
    pub fn http2(&mut self) -> &mut hyper::client::conn::http2::Builder<TokioExecutor> {
        &mut self.http2
    }
}

impl HttpConnectionBuilder {
    async fn handshake_h2(&self, stream: Stream) -> Result<HttpConnection, ConnectionError> {
        trace!("handshake");
        let (sender, conn) = self
            .http2
            .handshake(TokioIo::new(stream))
            .await
            .map_err(|error| ConnectionError::Handshake(error.into()))?;
        tokio::spawn(async {
            if let Err(err) = conn.await {
                if err.is_user() {
                    tracing::error!(%err, "h2 connection driver error");
                } else {
                    tracing::debug!(%err, "h2 connection driver error");
                }
            }
        });
        trace!("handshake complete");
        Ok(HttpConnection {
            inner: InnerConnection::H2(sender),
        })
    }

    async fn handshake_h1(&self, stream: Stream) -> Result<HttpConnection, ConnectionError> {
        trace!("handshake h1");
        let (sender, conn) = self
            .http1
            .handshake(TokioIo::new(stream))
            .await
            .map_err(|error| ConnectionError::Handshake(error.into()))?;
        tokio::spawn(async {
            if let Err(err) = conn.await {
                tracing::error!(%err, "h1 connection driver error");
            }
        });
        trace!("handshake complete");
        Ok(HttpConnection {
            inner: InnerConnection::H1(sender),
        })
    }

    #[tracing::instrument(skip_all, fields(protocol = ?self.protocol))]
    pub(crate) async fn handshake(
        &self,
        transport: TransportStream,
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

impl tower::Service<TransportStream> for HttpConnectionBuilder {
    type Response = HttpConnection;

    type Error = ConnectionError;

    type Future = future::HttpConnectFuture;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    #[tracing::instrument("http-connect", skip(self, req), fields(host = %req.host().unwrap_or("-")))]
    fn call(&mut self, req: TransportStream) -> Self::Future {
        future::HttpConnectFuture::new(self.clone(), req)
    }
}

impl tower::Service<TransportStream> for hyper::client::conn::http1::Builder {
    type Response = HttpConnection;

    type Error = ConnectionError;
    type Future = BoxFuture<'static, Result<HttpConnection, ConnectionError>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: TransportStream) -> Self::Future {
        let builder = self.clone();
        let stream: braid::client::Stream = req.into();

        Box::pin(async move {
            let (sender, conn) = builder
                .handshake(TokioIo::new(stream))
                .await
                .map_err(|err| ConnectionError::Handshake(err.into()))?;
            tokio::spawn(async {
                if let Err(err) = conn.await {
                    if err.is_user() {
                        tracing::error!(%err, "h1 connection driver error");
                    } else {
                        tracing::debug!(%err, "h1 connection driver error");
                    }
                }
            });
            Ok(HttpConnection {
                inner: InnerConnection::H1(sender),
            })
        })
    }
}

impl<E> tower::Service<TransportStream> for hyper::client::conn::http2::Builder<E>
where
    E: hyper::rt::bounds::Http2ClientConnExec<arnold::Body, TokioIo<braid::client::Stream>>
        + Unpin
        + Send
        + Sync
        + Clone
        + 'static,
{
    type Response = HttpConnection;

    type Error = ConnectionError;
    type Future = BoxFuture<'static, Result<HttpConnection, ConnectionError>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: TransportStream) -> Self::Future {
        let builder = self.clone();
        let stream: braid::client::Stream = req.into();

        Box::pin(async move {
            let (sender, conn) = builder
                .handshake(TokioIo::new(stream))
                .await
                .map_err(|err| ConnectionError::Handshake(err.into()))?;
            tokio::spawn(async {
                if let Err(err) = conn.await {
                    if err.is_user() {
                        tracing::error!(%err, "h2 connection driver error");
                    } else {
                        tracing::debug!(%err, "h2 connection driver error");
                    }
                }
            });
            Ok(HttpConnection {
                inner: InnerConnection::H2(sender),
            })
        })
    }
}

mod future {
    use std::fmt;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use futures_util::FutureExt;

    use crate::conn::TransportStream;
    use crate::DebugLiteral;

    use super::ConnectionError;
    use super::HttpConnection;
    use super::HttpConnectionBuilder;

    type BoxFuture<'a, T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

    pub struct HttpConnectFuture {
        future: BoxFuture<'static, HttpConnection, ConnectionError>,
    }

    impl fmt::Debug for HttpConnectFuture {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("HttpConnectFuture")
                .field("state", &DebugLiteral("Handshaking"))
                .finish()
        }
    }

    impl HttpConnectFuture {
        pub(super) fn new(
            builder: HttpConnectionBuilder,
            stream: TransportStream,
        ) -> HttpConnectFuture {
            let future = Box::pin(async move { builder.handshake(stream).await });

            Self { future }
        }
    }

    impl Future for HttpConnectFuture {
        type Output = Result<HttpConnection, ConnectionError>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.future.poll_unpin(cx)
        }
    }
}

/// An HTTP connection.
pub struct HttpConnection {
    inner: InnerConnection,
}

impl fmt::Debug for HttpConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpConnection")
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

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::future::poll_fn;
    use std::future::Future;

    use super::*;

    use crate::Error;

    use futures_util::{stream::StreamExt as _, FutureExt, TryFutureExt};
    use static_assertions::assert_impl_all;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tower::Service;

    type BoxError = Box<dyn std::error::Error + Send + Sync>;

    assert_impl_all!(HttpConnectionBuilder: Service<TransportStream, Response = HttpConnection, Error = ConnectionError, Future = future::HttpConnectFuture>, Debug, Clone);
    assert_impl_all!(future::HttpConnectFuture: Future<Output = Result<HttpConnection, ConnectionError>>, Debug, Send);

    async fn transport() -> Result<(TransportStream, Stream), BoxError> {
        let (client, mut incoming) = braid::duplex::pair("test".parse()?);

        let (tx, rx) = tokio::try_join!(
            async {
                let stream = client.connect(1024, None).await?;
                Ok::<_, BoxError>(TransportStream::new(stream.into()).await?)
            },
            async { Ok(incoming.next().await.ok_or("Acceptor closed")??) }
        )?;

        Ok((tx, rx.into()))
    }

    #[tokio::test]
    async fn http_connector_always_ready() {
        let mut builder = HttpConnectionBuilder::default();

        poll_fn(|cx| builder.poll_ready(cx))
            .now_or_never()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn http_connector_request_h1() {
        let _ = tracing_subscriber::fmt::try_init();

        let mut builder = HttpConnectionBuilder::default();

        let (stream, rx) = transport().await.unwrap();

        let mut conn = builder.call(stream).await.unwrap();
        conn.when_ready().await.unwrap();
        assert!(conn.is_open());
        assert!(!conn.can_share());
        assert_eq!(conn.version(), Version::HTTP_11);
        assert!(conn.reuse().is_none());
        assert_eq!(
            format!("{:?}", conn),
            "HttpConnection { version: HTTP/1.1 }"
        );

        let request = http::Request::builder()
            .method(http::Method::GET)
            .uri("http://localhost/")
            .body(arnold::Body::empty())
            .unwrap();

        let request_future = conn
            .send_request(request)
            .map_err(|err| Error::Transport(err.into()));

        let server_future = async move {
            let mut buf = String::new();
            let _ = BufReader::new(rx)
                .read_line(&mut buf)
                .await
                .map_err(|err| Error::Transport(err.into()))?;
            trace!(?buf, "received request");
            assert_eq!(buf, "GET http://localhost/ HTTP/1.1\r\n");
            Ok::<_, Error>(())
        };

        let (_rtx, rrx) = tokio::join!(request_future, server_future);

        assert!(rrx.is_ok());
    }

    #[tokio::test]
    async fn http_connector_request_h2() {
        let _ = tracing_subscriber::fmt::try_init();

        let mut builder = HttpConnectionBuilder::default();
        builder.set_protocol(HttpProtocol::Http2);

        let (stream, rx) = transport().await.unwrap();

        let mut conn = builder.call(stream).await.unwrap();
        conn.when_ready().await.unwrap();
        assert!(conn.is_open());
        assert!(conn.can_share());
        assert_eq!(conn.version(), Version::HTTP_2);
        assert!(conn.reuse().is_some());
        assert_eq!(
            format!("{:?}", conn),
            "HttpConnection { version: HTTP/2.0 }"
        );

        let request = http::Request::builder()
            .version(::http::Version::HTTP_2)
            .method(http::Method::GET)
            .uri("http://localhost/")
            .body(arnold::Body::empty())
            .unwrap();

        let request_future = conn
            .send_request(request)
            .map_err(|err| Error::Transport(err.into()));

        let server_future = async move {
            let mut buf = String::new();
            let _ = BufReader::new(rx)
                .read_line(&mut buf)
                .await
                .map_err(|err| Error::Transport(err.into()))?;
            trace!(?buf, "received request");
            assert_eq!(buf, "PRI * HTTP/2.0\r\n");
            Ok::<_, Error>(())
        };

        let (_rtx, rrx) = tokio::join!(request_future, server_future);

        assert!(rrx.is_ok());
    }
}
