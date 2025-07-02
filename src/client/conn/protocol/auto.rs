//! Auto-switching HTTP/1 and HTTP/2 connections.

use std::fmt;
use std::marker::PhantomData;
use std::pin::pin;

use http_body::Body;
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::trace;

use http_body::Body as HttpBody;

use crate::client::conn::connection::SendRequestFuture;
use crate::client::conn::Connection;
use crate::client::pool::PoolableConnection;

use crate::bridge::io::TokioIo;
use crate::bridge::rt::TokioExecutor;
use crate::client::conn::connection::ConnectionError;
use crate::info::HasConnectionInfo;
use crate::info::HasTlsConnectionInfo;
use crate::BoxError;

use super::HttpProtocol;
use super::ProtocolRequest;

/// An HTTP connection which is either HTTP1 or HTTP2
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

    type Future = SendRequestFuture;

    fn send_request(&mut self, mut request: http::Request<B>) -> Self::Future {
        match &mut self.inner {
            InnerConnection::H2(conn) => {
                *request.version_mut() = http::Version::HTTP_2;
                SendRequestFuture::new(conn.send_request(request))
            }
            InnerConnection::H1(conn) => {
                *request.version_mut() = http::Version::HTTP_11;
                SendRequestFuture::new(conn.send_request(request))
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

impl<B> PoolableConnection<B> for HttpConnection<B>
where
    B: HttpBody + Send + 'static,
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

/// A builder for configuring and starting HTTP connections.
///
/// This builder allows configuring the HTTP/1 and HTTP/2 settings for the connection,
/// and will pick the protocol based on the ALPN negotiation or the request's version
/// if ALPN is not available.
///
/// Since it takes advantage of ALPN, this protocol requires that the underlying IO
/// implements `HasTlsConnectionInfo` to provide the ALPN negotiation result. This can
/// be done by calling `TransportExt::with_tls` or `TransportExt::without_tls` on the
/// transport.
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
    <B as Body>::Error: Into<BoxError>,
{
    async fn handshake_h2<IO>(&self, stream: IO) -> Result<HttpConnection<B>, ConnectionError>
    where
        IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        trace!("handshake h2");
        // let span = tracing::info_span!(parent: tracing::Span::none(), "connection", version = ?http::Version::HTTP_2, peer = %stream.info().remote_addr());
        let (sender, conn) = self
            .http2
            .handshake(TokioIo::new(stream))
            .await
            .map_err(|error| ConnectionError::Handshake(error.into()))?;
        tokio::spawn(async {
            let conn = pin!(conn);
            if let Err(err) = conn.await {
                if err.is_user() {
                    tracing::error!(err = format!("{err:#?}"), "h2 connection driver error");
                } else if !err.is_closed() {
                    tracing::debug!(err = format!("{err:#?}"), "h2 connection driver error");
                } else {
                    tracing::trace!(err = format!("{err:#?}"), "h2 connection closed")
                }
            }
        });
        trace!("handshake complete");
        Ok(HttpConnection::h2(sender))
    }

    async fn handshake_h1<IO>(&self, stream: IO) -> Result<HttpConnection<B>, ConnectionError>
    where
        IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        trace!(version = ?http::Version::HTTP_11, peer = %stream.info().remote_addr(), "handshake h1");
        // let span = tracing::info_span!("connection", version = ?http::Version::HTTP_11, peer = %stream.info().remote_addr());

        let (sender, conn) = self
            .http1
            .handshake(TokioIo::new(stream))
            .await
            .map_err(|error| ConnectionError::Handshake(error.into()))?;
        tokio::spawn(async {
            if let Err(err) = conn.with_upgrades().await {
                tracing::error!(err = format!("{err:#}"), "h1 connection driver error");
            }
        });
        trace!("handshake complete");
        Ok(HttpConnection::h1(sender))
    }

    #[tracing::instrument(level = "trace", name = "handshake", skip_all)]
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
                if transport.tls_info().and_then(|tls| tls.alpn.as_ref())
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
    <B as Body>::Error: Into<BoxError>,
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

    #[tracing::instrument("http-connect", level="trace", skip_all, fields(addr=?req.transport.info().remote_addr()))]
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
    use crate::BoxError;
    use crate::DebugLiteral;

    use super::HttpConnection;
    use super::HttpConnectionBuilder;

    type BoxFuture<'a, T, E> = crate::BoxFuture<'a, Result<T, E>>;

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
        <B as Body>::Error: Into<BoxError>,
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

#[cfg(all(test, feature = "stream"))]
mod tests {
    use std::fmt::Debug;
    use std::future::Future;

    use super::*;

    use crate::client::conn::connection::ConnectionError;

    use crate::client::conn::Protocol as _;
    use crate::client::conn::{protocol::HttpProtocol, Stream};
    use crate::client::Error;

    #[cfg(all(feature = "mocks", feature = "tls"))]
    use crate::stream::tls::TlsHandshakeStream as _;

    use futures_util::{stream::StreamExt as _, TryFutureExt};
    use http::Version;
    use static_assertions::assert_impl_all;
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[cfg(feature = "mocks")]
    use tower::Service;

    type BoxError = Box<dyn std::error::Error + Send + Sync>;

    #[cfg(all(feature = "mocks", feature = "stream"))]
    assert_impl_all!(HttpConnectionBuilder<crate::Body>: Service<ProtocolRequest<Stream, crate::Body>, Response = HttpConnection<crate::Body>, Error = ConnectionError, Future = future::HttpConnectFuture<Stream, crate::Body>>, Debug, Clone);

    #[cfg(feature = "stream")]
    assert_impl_all!(future::HttpConnectFuture<Stream, crate::Body>: Future<Output = Result<HttpConnection<crate::Body>, ConnectionError>>, Debug, Send);

    #[cfg(feature = "stream")]
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
    #[cfg(feature = "stream")]
    async fn http_connector_request_h1() {
        use crate::client::conn::connection::ConnectionExt as _;

        let _ = tracing_subscriber::fmt::try_init();

        let mut builder = HttpConnectionBuilder::default();

        let (stream, rx) = transport().await.unwrap();

        let mut conn = builder.connect(stream, HttpProtocol::Http1).await.unwrap();
        conn.when_ready().await.unwrap();
        assert!(conn.is_open());
        assert!(!conn.can_share());
        assert_eq!(conn.version(), Version::HTTP_11);
        assert!(conn.reuse().is_none());
        assert_eq!(format!("{conn:?}"), "HttpConnection { version: HTTP/1.1 }");

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
    #[cfg(all(feature = "stream", feature = "tls"))]
    async fn http_connector_request_h2() {
        use crate::client::conn::connection::ConnectionExt as _;

        let _ = tracing_subscriber::fmt::try_init();

        let mut builder = HttpConnectionBuilder::default();

        let (stream, rx) = transport().await.unwrap();

        let mut conn = builder.connect(stream, HttpProtocol::Http2).await.unwrap();
        conn.when_ready().await.unwrap();
        assert!(conn.is_open());
        assert!(conn.can_share());
        assert_eq!(conn.version(), Version::HTTP_2);
        assert!(conn.reuse().is_some());
        assert_eq!(format!("{conn:?}"), "HttpConnection { version: HTTP/2.0 }");

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

    #[cfg(all(feature = "mocks", feature = "stream", feature = "tls"))]
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

        async fn handshake(mut stream: Stream) -> Result<(), std::io::Error> {
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
        assert_eq!(format!("{conn:?}"), "HttpConnection { version: HTTP/2.0 }");
    }
}
