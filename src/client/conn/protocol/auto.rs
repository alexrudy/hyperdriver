//! Auto-switching HTTP/1 and HTTP/2 connections.

use std::fmt;
use std::marker::PhantomData;
use std::pin::pin;

use http_body::Body;
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::trace;

use http_body::Body as HttpBody;

use crate::client::conn::connection::SendRequestFuture;
use crate::service::HttpConnectionInfo;
use chateau::client::conn::Connection;
use chateau::client::pool::PoolableConnection;

use crate::bridge::io::TokioIo;
use crate::bridge::rt::TokioExecutor;
use crate::BoxError;
use chateau::info::HasConnectionInfo;
use chateau::info::HasTlsConnectionInfo;

use super::HttpProtocol;

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

    /// Get the HTTP protocol version
    pub fn version(&self) -> HttpProtocol {
        match &self.inner {
            InnerConnection::H1(_) => HttpProtocol::Http1,
            InnerConnection::H2(_) => HttpProtocol::Http2,
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

impl<B> Connection<http::Request<B>> for HttpConnection<B>
where
    B: HttpBody + Send + 'static,
{
    type Response = http::Response<hyper::body::Incoming>;

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
}

impl<B> PoolableConnection<http::Request<B>> for HttpConnection<B>
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

impl<B> HttpConnectionInfo<B> for HttpConnection<B>
where
    B: HttpBody + Send + 'static,
{
    fn version(&self) -> HttpProtocol {
        match &self.inner {
            InnerConnection::H1(_) => HttpProtocol::Http1,
            InnerConnection::H2(_) => HttpProtocol::Http2,
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
pub struct AlpnHttpConnectionBuilder<B> {
    http1: hyper::client::conn::http1::Builder,
    http2: hyper::client::conn::http2::Builder<TokioExecutor>,
    _body: PhantomData<fn(B) -> ()>,
}

impl<B> Clone for AlpnHttpConnectionBuilder<B> {
    fn clone(&self) -> Self {
        Self {
            http1: self.http1.clone(),
            http2: self.http2.clone(),
            _body: PhantomData,
        }
    }
}

impl<B> AlpnHttpConnectionBuilder<B> {
    /// Get the HTTP/1.1 configuration.
    pub fn http1(&mut self) -> &mut hyper::client::conn::http1::Builder {
        &mut self.http1
    }

    /// Get the HTTP/2 configuration.
    pub fn http2(&mut self) -> &mut hyper::client::conn::http2::Builder<TokioExecutor> {
        &mut self.http2
    }
}

impl<B> AlpnHttpConnectionBuilder<B>
where
    B: Body + Unpin + Send + 'static,
    <B as Body>::Data: Send,
    <B as Body>::Error: Into<BoxError>,
{
    async fn handshake_h2<IO>(&self, stream: IO) -> Result<HttpConnection<B>, hyper::Error>
    where
        IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        trace!("handshake h2");
        // let span = tracing::info_span!(parent: tracing::Span::none(), "connection", version = ?http::Version::HTTP_2, peer = %stream.info().remote_addr());
        let (sender, conn) = self.http2.handshake(TokioIo::new(stream)).await?;
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

    async fn handshake_h1<IO>(&self, stream: IO) -> Result<HttpConnection<B>, hyper::Error>
    where
        IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        trace!(version = ?http::Version::HTTP_11, peer = %stream.info().remote_addr(), "handshake h1");
        // let span = tracing::info_span!("connection", version = ?http::Version::HTTP_11, peer = %stream.info().remote_addr());

        let (sender, conn) = self.http1.handshake(TokioIo::new(stream)).await?;
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
    ) -> Result<HttpConnection<B>, hyper::Error>
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
                if transport.tls_info().and_then(|tls| tls.alpn.as_deref()) == Some("h2") {
                    trace!("alpn h2 switching");
                    return self.handshake_h2(transport).await;
                }

                #[cfg(feature = "tls")]
                trace!(tls=?transport.tls_info(), "no alpn h2 switching");

                self.handshake_h1(transport).await
            }
            HttpProtocol::Http3 => {
                //TODO: This should return a protocol error?
                panic!("Protocol not supported");
            }
        }
    }
}

impl<B> Default for AlpnHttpConnectionBuilder<B> {
    fn default() -> Self {
        Self {
            http1: hyper::client::conn::http1::Builder::new(),
            http2: hyper::client::conn::http2::Builder::new(TokioExecutor::new()),
            _body: PhantomData,
        }
    }
}

impl<IO, B> tower::Service<IO> for AlpnHttpConnectionBuilder<B>
where
    IO: HasTlsConnectionInfo + HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    IO::Addr: Clone + Send + Sync,
    B: Body + Unpin + Send + 'static,
    <B as Body>::Data: Send,
    <B as Body>::Error: Into<BoxError>,
{
    type Response = HttpConnection<B>;

    type Error = hyper::Error;

    type Future = future::HttpConnectFuture<IO, B>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    #[tracing::instrument("http-connect", level="trace", skip_all, fields(addr=?req.info().remote_addr()))]
    fn call(&mut self, req: IO) -> Self::Future {
        let builder = std::mem::replace(self, self.clone());
        //TODO: Should there be some way to force an HTTP2 connection from the get-go?
        // Since this could depend on the HTTP request, we'd need a different way to propogate that along with the
        // IO stream into this method.
        future::HttpConnectFuture::new(builder, req, HttpProtocol::Http1)
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

    use crate::client::conn::protocol::HttpProtocol;
    use crate::BoxError;
    use crate::DebugLiteral;
    use chateau::info::{HasConnectionInfo, HasTlsConnectionInfo};

    use super::AlpnHttpConnectionBuilder;
    use super::HttpConnection;

    type BoxFuture<'a, T, E> = crate::BoxFuture<'a, Result<T, E>>;

    pub struct HttpConnectFuture<IO, B> {
        future: BoxFuture<'static, HttpConnection<B>, hyper::Error>,
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
            builder: AlpnHttpConnectionBuilder<B>,
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
        type Output = Result<HttpConnection<B>, hyper::Error>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.future.poll_unpin(cx)
        }
    }
}

#[cfg(all(test, feature = "stream"))]
mod tests {
    use std::fmt::Debug;
    use std::future::Future;
    use std::sync::Arc;

    use chateau::client::conn::connection::ConnectionExt as _;
    use chateau::stream::duplex::DuplexStream;
    use futures_util::{stream::StreamExt as _, TryFutureExt};
    use rustls::pki_types::ServerName;
    use static_assertions::assert_impl_all;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tracing_test::traced_test;

    use crate::client::conn::stream::mock::MockTls;
    use crate::client::conn::Protocol as _;
    use crate::client::conn::{protocol::HttpProtocol, Stream};
    use crate::client::Error;

    use super::*;

    #[cfg(feature = "mocks")]
    use tower::Service;

    type BoxError = Box<dyn std::error::Error + Send + Sync>;

    #[cfg(all(feature = "mocks", feature = "stream"))]
    assert_impl_all!(AlpnHttpConnectionBuilder<crate::Body>: Service<Stream, Response = HttpConnection<crate::Body>, Error = hyper::Error, Future = future::HttpConnectFuture<Stream, crate::Body>>, Debug, Clone);

    #[cfg(feature = "stream")]
    assert_impl_all!(future::HttpConnectFuture<Stream, crate::Body>: Future<Output = Result<HttpConnection<crate::Body>, hyper::Error>>, Debug, Send);

    #[cfg(feature = "stream")]
    async fn transport() -> Result<(DuplexStream, DuplexStream), BoxError> {
        let (client, mut incoming) = chateau::stream::duplex::pair();

        // Connect to the server. This is a helper function that creates a client and server
        // pair, and returns the client's transport stream and the server's stream.
        let (tx, rx) = tokio::try_join!(
            async {
                let stream = client.connect(1024).await?;
                Ok::<_, BoxError>(stream)
            },
            async { Ok(incoming.next().await.ok_or("Acceptor closed")??) }
        )?;

        Ok((tx, rx))
    }

    fn tls_info(alpn_h2: bool) -> chateau::info::TlsConnectionInfo {
        let config = Arc::new(crate::fixtures::tls_client_config());
        let server = ServerName::try_from("example.com").unwrap();
        if alpn_h2 {
            let info = rustls::ClientConnection::new_with_alpn(config, server, vec![b"h2".into()])
                .unwrap();
            let mut info = chateau::info::TlsConnectionInfo::client(&info);
            info.alpn = Some("h2".into());
            info.server_name = Some("example.com".into());
            info.validated_server_name = true;
            info
        } else {
            let info = rustls::ClientConnection::new(config, server).unwrap();
            let mut info = chateau::info::TlsConnectionInfo::client(&info);
            info.server_name = Some("example.com".into());
            info
        }
    }

    #[tokio::test]
    #[traced_test]
    #[cfg(feature = "stream")]
    async fn http_connector_request_h1() {
        let mut builder = AlpnHttpConnectionBuilder::default();

        let (stream, rx) = transport().await.unwrap();

        let mut conn = builder
            .connect(MockTls::new(stream, tls_info(false)))
            .await
            .unwrap();
        conn.when_ready().await.unwrap();
        assert!(conn.is_open());
        assert_eq!(conn.version(), HttpProtocol::Http1);
        assert!(!conn.can_share());
        assert!(conn.reuse().is_none());

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
    #[traced_test]
    #[cfg(all(feature = "mocks", feature = "stream", feature = "tls"))]
    async fn http_connector_alpn_h2() {
        let (server, client) = transport().await.unwrap();

        let mut builder = AlpnHttpConnectionBuilder::default();

        let mut conn = builder
            .connect(MockTls::new(client, tls_info(true)))
            .await
            .unwrap();
        conn.when_ready().await.unwrap();

        assert!(conn.is_open());
        assert_eq!(conn.version(), HttpProtocol::Http2);
        assert!(conn.can_share());
        assert!(conn.reuse().is_some());

        let request = http::Request::builder()
            .method(http::Method::GET)
            .version(http::Version::HTTP_2)
            .uri("http://localhost/")
            .body(crate::body::Body::empty())
            .unwrap();

        let request_future = conn
            .send_request(request)
            .map_err(|err| Error::Transport(err.into()));

        let server_future = async move {
            let mut buf = String::new();
            let _ = BufReader::new(server)
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
