//! HTTP connection handling.

use ::http::{Response, Version};
use futures_util::future::BoxFuture;
use hyper::body::Incoming;
use std::fmt;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::trace;

use crate::bridge::io::TokioIo;
use crate::bridge::rt::TokioExecutor;
use crate::client::conn::Connection;
use crate::client::conn::TransportStream;
use crate::client::pool::PoolableConnection;
use crate::info::HasConnectionInfo;

use super::HttpProtocol;
use super::ProtocolRequest;

/// A builder for configuring and starting HTTP connections.
#[derive(Debug, Clone)]
pub struct HttpConnectionBuilder {
    http1: hyper::client::conn::http1::Builder,
    http2: hyper::client::conn::http2::Builder<TokioExecutor>,
}

impl HttpConnectionBuilder {
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
    async fn handshake_h2<IO>(&self, stream: IO) -> Result<HttpConnection, ConnectionError>
    where
        IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        trace!("handshake h2");
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

    async fn handshake_h1<IO>(&self, stream: IO) -> Result<HttpConnection, ConnectionError>
    where
        IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
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

    #[tracing::instrument(name = "tls", skip_all)]
    pub(crate) async fn handshake<IO>(
        &self,
        transport: TransportStream<IO>,
        protocol: HttpProtocol,
    ) -> Result<HttpConnection, ConnectionError>
    where
        IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        match protocol {
            HttpProtocol::Http2 => self.handshake_h2(transport.into_inner()).await,
            HttpProtocol::Http1 => {
                #[cfg(feature = "tls")]
                if transport
                    .info()
                    .tls
                    .as_ref()
                    .and_then(|tls| tls.alpn.as_ref())
                    == Some(&crate::stream::info::Protocol::Http(http::Version::HTTP_2))
                {
                    trace!("alpn h2 switching");
                    return self.handshake_h2(transport.into_inner()).await;
                }

                #[cfg(feature = "tls")]
                trace!(tls=?transport.info().tls, "no alpn h2 switching");

                self.handshake_h1(transport.into_inner()).await
            }
        }
    }
}

impl Default for HttpConnectionBuilder {
    fn default() -> Self {
        Self {
            http1: hyper::client::conn::http1::Builder::new(),
            http2: hyper::client::conn::http2::Builder::new(TokioExecutor::new()),
        }
    }
}

impl<IO> tower::Service<ProtocolRequest<IO>> for HttpConnectionBuilder
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    IO::Addr: Send + Sync,
{
    type Response = HttpConnection;

    type Error = ConnectionError;

    type Future = future::HttpConnectFuture<IO>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    #[tracing::instrument("http-connect", skip(self, req), fields(host = %req.transport.host().unwrap_or("-")))]
    fn call(&mut self, req: ProtocolRequest<IO>) -> Self::Future {
        future::HttpConnectFuture::new(self.clone(), req.transport, req.version)
    }
}

impl<IO> tower::Service<ProtocolRequest<IO>> for hyper::client::conn::http1::Builder
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
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

    fn call(&mut self, req: ProtocolRequest<IO>) -> Self::Future {
        let builder = self.clone();
        let stream = req.transport.into_inner();

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

impl<E, IO> tower::Service<ProtocolRequest<IO>> for hyper::client::conn::http2::Builder<E>
where
    E: hyper::rt::bounds::Http2ClientConnExec<crate::body::Body, TokioIo<IO>>
        + Unpin
        + Send
        + Sync
        + Clone
        + 'static,
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
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

    fn call(&mut self, req: ProtocolRequest<IO>) -> Self::Future {
        let builder = self.clone();
        let stream = req.transport.into_inner();

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
    use std::marker::PhantomData;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use futures_util::FutureExt;
    use tokio::io::{AsyncRead, AsyncWrite};

    use crate::client::conn::TransportStream;
    use crate::client::HttpProtocol;
    use crate::stream::info::HasConnectionInfo;
    use crate::DebugLiteral;

    use super::ConnectionError;
    use super::HttpConnection;
    use super::HttpConnectionBuilder;

    type BoxFuture<'a, T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

    pub struct HttpConnectFuture<IO> {
        future: BoxFuture<'static, HttpConnection, ConnectionError>,
        _io: PhantomData<IO>,
    }

    impl<IO> fmt::Debug for HttpConnectFuture<IO> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("HttpConnectFuture")
                .field("state", &DebugLiteral("Handshaking"))
                .finish()
        }
    }

    impl<IO> HttpConnectFuture<IO>
    where
        IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
        IO::Addr: Send + Sync,
    {
        pub(super) fn new(
            builder: HttpConnectionBuilder,
            stream: TransportStream<IO>,
            protocol: HttpProtocol,
        ) -> HttpConnectFuture<IO> {
            let future = Box::pin(async move { builder.handshake(stream, protocol).await });

            Self {
                future,
                _io: PhantomData,
            }
        }
    }

    impl<IO> Future for HttpConnectFuture<IO>
    where
        IO: Unpin,
    {
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
    H2(hyper::client::conn::http2::SendRequest<crate::body::Body>),
    H1(hyper::client::conn::http1::SendRequest<crate::body::Body>),
}

impl Connection for HttpConnection {
    type Error = hyper::Error;

    type Future = BoxFuture<'static, Result<Response<Incoming>, hyper::Error>>;

    fn send_request(&mut self, request: crate::body::Request) -> Self::Future {
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
    use std::future::Future;

    use super::*;

    use crate::client::{Error, Protocol};
    use crate::stream::client::Stream;

    use futures_util::{stream::StreamExt as _, TryFutureExt};
    use static_assertions::assert_impl_all;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tower::Service;

    type BoxError = Box<dyn std::error::Error + Send + Sync>;

    assert_impl_all!(HttpConnectionBuilder: Service<ProtocolRequest<Stream>, Response = HttpConnection, Error = ConnectionError, Future = future::HttpConnectFuture<Stream>>, Debug, Clone);
    assert_impl_all!(future::HttpConnectFuture<Stream>: Future<Output = Result<HttpConnection, ConnectionError>>, Debug, Send);

    async fn transport() -> Result<(TransportStream<Stream>, Stream), BoxError> {
        let (client, mut incoming) = crate::stream::duplex::pair("test".parse()?);

        let (tx, rx) = tokio::try_join!(
            async {
                let stream = client.connect(1024, None).await?;
                Ok::<_, BoxError>(TransportStream::new_stream(stream.into()).await?)
            },
            async { Ok(incoming.next().await.ok_or("Acceptor closed")??) }
        )?;

        Ok((tx, rx.into()))
    }

    #[tokio::test]
    async fn http_connector_request_h1() {
        let _ = tracing_subscriber::fmt::try_init();

        let mut builder = HttpConnectionBuilder::default();

        let (stream, rx) = transport().await.unwrap();

        let mut conn = builder.connect(stream, HttpProtocol::Http1).await.unwrap();
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
            .body(crate::body::Body::empty())
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

        let (stream, rx) = transport().await.unwrap();

        let mut conn = builder.connect(stream, HttpProtocol::Http2).await.unwrap();
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
            .body(crate::body::Body::empty())
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
