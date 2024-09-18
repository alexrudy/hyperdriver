//! Auto-switching HTTP/1 and HTTP/2 connections.

use std::marker::PhantomData;
use std::pin::pin;

use http_body::Body;
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::trace;
use tracing::Instrument as _;

use crate::bridge::io::TokioIo;
use crate::bridge::rt::TokioExecutor;
use crate::client::conn::connection::ConnectionError;
use crate::client::conn::connection::HttpConnection;
use crate::info::HasConnectionInfo;
use crate::info::HasTlsConnectionInfo;

use super::HttpProtocol;
use super::ProtocolRequest;

/// A builder for configuring and starting HTTP connections.
#[derive(Debug)]
pub struct HttpConnectionBuilder<B> {
    http1: hyper::client::conn::http1::Builder,
    http2: hyper::client::conn::http2::Builder<TokioExecutor>,
    _body: PhantomData<fn(B) -> ()>,
}

impl<B> Clone for HttpConnectionBuilder<B> {
    fn clone(&self) -> Self {
        Self {
            http1: self.http1.clone(),
            http2: self.http2.clone(),
            _body: PhantomData,
        }
    }
}

impl<B> HttpConnectionBuilder<B> {
    /// Get the HTTP/1.1 configuration.
    pub fn http1(&mut self) -> &mut hyper::client::conn::http1::Builder {
        &mut self.http1
    }

    /// Get the HTTP/2 configuration.
    pub fn http2(&mut self) -> &mut hyper::client::conn::http2::Builder<TokioExecutor> {
        &mut self.http2
    }
}

impl<B> HttpConnectionBuilder<B>
where
    B: Body + Unpin + Send + 'static,
    <B as Body>::Data: Send,
    <B as Body>::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    async fn handshake_h2<IO>(&self, stream: IO) -> Result<HttpConnection<B>, ConnectionError>
    where
        IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        trace!("handshake h2");
        let span = tracing::info_span!("connection", version = ?http::Version::HTTP_2, peer = %stream.info().remote_addr());
        let (sender, conn) = self
            .http2
            .handshake(TokioIo::new(stream))
            .await
            .map_err(|error| ConnectionError::Handshake(error.into()))?;
        tokio::spawn(
            async {
                let conn = pin!(conn);
                if let Err(err) = conn.await {
                    if err.is_user() {
                        tracing::error!(err = format!("{err:#}"), "h2 connection driver error");
                    } else {
                        tracing::debug!(err = format!("{err:#}"), "h2 connection driver error");
                    }
                }
            }
            .instrument(span),
        );
        trace!("handshake complete");
        Ok(HttpConnection::h2(sender))
    }

    async fn handshake_h1<IO>(&self, stream: IO) -> Result<HttpConnection<B>, ConnectionError>
    where
        IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        trace!("handshake h1");
        let span = tracing::info_span!("connection", version = ?http::Version::HTTP_11, peer = %stream.info().remote_addr());

        let (sender, conn) = self
            .http1
            .handshake(TokioIo::new(stream))
            .await
            .map_err(|error| ConnectionError::Handshake(error.into()))?;
        tokio::spawn(
            async {
                if let Err(err) = conn.await {
                    tracing::error!(err = format!("{err:#}"), "h1 connection driver error");
                }
            }
            .instrument(span),
        );
        trace!("handshake complete");
        Ok(HttpConnection::h1(sender))
    }

    #[tracing::instrument(name = "handshake", skip_all)]
    pub(crate) async fn handshake<IO>(
        &self,
        transport: IO,
        protocol: HttpProtocol,
    ) -> Result<HttpConnection<B>, ConnectionError>
    where
        IO: HasTlsConnectionInfo
            + HasConnectionInfo
            + AsyncRead
            + AsyncWrite
            + Send
            + Unpin
            + 'static,
        <IO as HasConnectionInfo>::Addr: Clone,
    {
        match protocol {
            HttpProtocol::Http2 => self.handshake_h2(transport).await,
            HttpProtocol::Http1 => {
                #[cfg(feature = "tls")]
                if transport
                    .tls_info()
                    .as_ref()
                    .and_then(|tls| tls.alpn.as_ref())
                    == Some(&crate::info::Protocol::Http(http::Version::HTTP_2))
                {
                    trace!("alpn h2 switching");
                    return self.handshake_h2(transport).await;
                }

                #[cfg(feature = "tls")]
                trace!(tls=?transport.tls_info(), "no alpn h2 switching");

                self.handshake_h1(transport).await
            }
        }
    }
}

impl<B> Default for HttpConnectionBuilder<B> {
    fn default() -> Self {
        Self {
            http1: hyper::client::conn::http1::Builder::new(),
            http2: hyper::client::conn::http2::Builder::new(TokioExecutor::new()),
            _body: PhantomData,
        }
    }
}

impl<IO, B> tower::Service<ProtocolRequest<IO, B>> for HttpConnectionBuilder<B>
where
    IO: HasTlsConnectionInfo + HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    IO::Addr: Clone + Send + Sync,
    B: Body + Unpin + Send + 'static,
    <B as Body>::Data: Send,
    <B as Body>::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Response = HttpConnection<B>;

    type Error = ConnectionError;

    type Future = future::HttpConnectFuture<IO, B>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    #[tracing::instrument("http-connect", skip_all, fields(addr=?req.transport.info().remote_addr()))]
    fn call(&mut self, req: ProtocolRequest<IO, B>) -> Self::Future {
        let builder = std::mem::replace(self, self.clone());
        future::HttpConnectFuture::new(builder, req.transport, req.version)
    }
}

mod future {
    use std::fmt;
    use std::future::Future;
    use std::marker::PhantomData;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use futures_util::FutureExt;
    use http_body::Body;
    use tokio::io::{AsyncRead, AsyncWrite};

    use crate::client::conn::connection::ConnectionError;
    use crate::client::conn::protocol::HttpProtocol;
    use crate::info::{HasConnectionInfo, HasTlsConnectionInfo};
    use crate::DebugLiteral;

    use super::HttpConnection;
    use super::HttpConnectionBuilder;

    type BoxFuture<'a, T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

    pub struct HttpConnectFuture<IO, B> {
        future: BoxFuture<'static, HttpConnection<B>, ConnectionError>,
        _io: PhantomData<IO>,
    }

    impl<IO, B> fmt::Debug for HttpConnectFuture<IO, B> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("HttpConnectFuture")
                .field("state", &DebugLiteral("Handshaking"))
                .finish()
        }
    }

    impl<IO, B> HttpConnectFuture<IO, B>
    where
        IO: HasTlsConnectionInfo
            + HasConnectionInfo
            + AsyncRead
            + AsyncWrite
            + Send
            + Unpin
            + 'static,
        IO::Addr: Clone + Send + Sync,
        B: Body + Unpin + Send + 'static,
        <B as Body>::Data: Send,
        <B as Body>::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        pub(super) fn new(
            builder: HttpConnectionBuilder<B>,
            stream: IO,
            protocol: HttpProtocol,
        ) -> HttpConnectFuture<IO, B> {
            let future = Box::pin(async move { builder.handshake(stream, protocol).await });

            Self {
                future,
                _io: PhantomData,
            }
        }
    }

    impl<IO, B> Future for HttpConnectFuture<IO, B>
    where
        IO: Unpin,
    {
        type Output = Result<HttpConnection<B>, ConnectionError>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.future.poll_unpin(cx)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::future::Future;
    use std::io;

    use super::*;

    use crate::client::conn::connection::ConnectionError;
    use crate::client::conn::Protocol as _;
    use crate::client::conn::{protocol::HttpProtocol, Connection as _, Stream};
    use crate::client::pool::PoolableConnection as _;
    use crate::client::Error;
    use crate::stream::tls::TlsHandshakeStream as _;

    use futures_util::{stream::StreamExt as _, TryFutureExt};
    use http::Version;
    use static_assertions::assert_impl_all;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tower::Service;

    type BoxError = Box<dyn std::error::Error + Send + Sync>;

    assert_impl_all!(HttpConnectionBuilder<crate::Body>: Service<ProtocolRequest<Stream, crate::Body>, Response = HttpConnection<crate::Body>, Error = ConnectionError, Future = future::HttpConnectFuture<Stream, crate::Body>>, Debug, Clone);
    assert_impl_all!(future::HttpConnectFuture<Stream, crate::Body>: Future<Output = Result<HttpConnection<crate::Body>, ConnectionError>>, Debug, Send);

    async fn transport() -> Result<(Stream, Stream), BoxError> {
        let (client, mut incoming) = crate::stream::duplex::pair();

        // Connect to the server. This is a helper function that creates a client and server
        // pair, and returns the client's transport stream and the server's stream.
        let (tx, rx) = tokio::try_join!(
            async {
                let stream = client.connect(1024).await?;
                Ok::<_, BoxError>(stream)
            },
            async { Ok(incoming.next().await.ok_or("Acceptor closed")??) }
        )?;

        Ok((tx.into(), rx.into()))
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

    #[cfg(feature = "mocks")]
    #[tokio::test]
    async fn http_connector_alpn_h2() {
        use crate::client::conn::stream::mock::MockTls;

        let _ = tracing_subscriber::fmt::try_init();

        let (stream, client) = transport().await.unwrap();

        async fn serve(stream: Stream) -> Result<HttpConnection<crate::Body>, ConnectionError> {
            let mut builder = HttpConnectionBuilder::default();

            let tls = crate::info::TlsConnectionInfo::new_client(Some(
                crate::info::Protocol::http(http::Version::HTTP_2),
            ));

            builder
                .connect(MockTls::new(stream, tls), HttpProtocol::Http1)
                .await
        }

        async fn handshake(mut stream: Stream) -> Result<(), io::Error> {
            stream.finish_handshake().await?;
            tracing::info!("Client finished handshake");
            Ok(())
        }

        let (conn, client) = tokio::join!(serve(stream), handshake(client));
        client.unwrap();

        let mut conn = conn.unwrap();
        assert!(conn.is_open());
        assert!(conn.can_share());
        assert_eq!(conn.version(), Version::HTTP_2);
        assert!(conn.reuse().is_some());
        assert_eq!(
            format!("{:?}", conn),
            "HttpConnection { version: HTTP/2.0 }"
        );
    }
}
