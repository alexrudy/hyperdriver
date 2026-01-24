//! Protocol describes how http requests and responses are transmitted over a connection.
//!
//! There are three protocols provided here: HTTP/1.1, HTTP/2, and an automatically
//! negotiated protocol which can be either HTTP/1.1 or HTTP/2 based on the connection
//! protocol and ALPN negotiation.

use std::marker::PhantomData;
use std::ops::Deref;
use std::ops::DerefMut;

use http_body::Body;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use crate::BoxError;
use crate::bridge::io::TokioIo;
use chateau::info::HasConnectionInfo;

pub mod auto;
#[cfg(feature = "mocks")]
pub mod mock;
pub use hyper::client::conn::http1;
pub use hyper::client::conn::http2;

use super::connection::{Http1Connection, Http2Connection};
use crate::info::HttpProtocol;

/// Wrapper for hyper's HTTP/1 connection builder for compatibility with Chateau
#[derive(Debug)]
pub struct Http1Builder<B>(hyper::client::conn::http1::Builder, PhantomData<fn(B)>);

impl<B> Http1Builder<B> {
    /// Construct a new HTTP/1 connection builder with default settings
    pub fn new() -> Self {
        Http1Builder(hyper::client::conn::http1::Builder::new(), PhantomData)
    }
}

impl<B> From<hyper::client::conn::http1::Builder> for Http1Builder<B> {
    fn from(value: hyper::client::conn::http1::Builder) -> Self {
        Self(value, PhantomData)
    }
}

impl<B> Default for Http1Builder<B> {
    fn default() -> Self {
        Self(
            hyper::client::conn::http1::Builder::new(),
            Default::default(),
        )
    }
}

impl<B> Clone for Http1Builder<B> {
    fn clone(&self) -> Self {
        Http1Builder(self.0.clone(), PhantomData)
    }
}

impl<B> Deref for Http1Builder<B> {
    type Target = hyper::client::conn::http1::Builder;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<B> DerefMut for Http1Builder<B> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<IO, B> tower::Service<IO> for Http1Builder<B>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    B: Body + Unpin + Send + 'static,
    <B as Body>::Data: Send,
    <B as Body>::Error: Into<BoxError>,
{
    type Response = Http1Connection<B>;

    type Error = hyper::Error;
    type Future = self::future::HttpProtocolFuture<Http1Connection<B>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: IO) -> Self::Future {
        let builder = std::mem::replace(self, self.clone());

        // let info = stream.info();
        // let span = tracing::info_span!("connection", version=?http::Version::HTTP_11, peer=%info.remote_addr());

        self::future::HttpProtocolFuture::new(async move {
            let (sender, conn) = builder.handshake(TokioIo::new(req)).await?;

            tokio::spawn(async {
                if let Err(err) = conn.with_upgrades().await {
                    if err.is_user() {
                        tracing::error!(err = format!("{err:#}"), "h1 connection driver error");
                    } else {
                        tracing::debug!(err = format!("{err:#}"), "h1 connection driver error");
                    }
                }
            });
            Ok(Http1Connection::new(sender))
        })
    }
}

/// Wrapper type for hyper's HTTP/2 builder for compatibility with chateau.
#[derive(Debug)]
pub struct Http2Builder<B, E> {
    builder: hyper::client::conn::http2::Builder<E>,
    body: PhantomData<fn(B)>,
}

impl<B, E> Http2Builder<B, E>
where
    E: Clone,
{
    /// Construct a new HTTP/2 connection builder with default settings
    pub fn new(executor: E) -> Self {
        Self {
            builder: hyper::client::conn::http2::Builder::new(executor),
            body: PhantomData,
        }
    }
}

impl<B, E> From<hyper::client::conn::http2::Builder<E>> for Http2Builder<B, E> {
    fn from(value: hyper::client::conn::http2::Builder<E>) -> Self {
        Self {
            builder: value,
            body: PhantomData,
        }
    }
}

impl<B, E: Clone> Clone for Http2Builder<B, E> {
    fn clone(&self) -> Self {
        Self {
            builder: self.builder.clone(),
            body: PhantomData,
        }
    }
}

impl<B, E> Deref for Http2Builder<B, E> {
    type Target = hyper::client::conn::http2::Builder<E>;

    fn deref(&self) -> &Self::Target {
        &self.builder
    }
}

impl<B, E> DerefMut for Http2Builder<B, E> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.builder
    }
}

impl<E, IO, BIn> tower::Service<IO> for Http2Builder<BIn, E>
where
    E: hyper::rt::bounds::Http2ClientConnExec<BIn, TokioIo<IO>>
        + Unpin
        + Send
        + Sync
        + Clone
        + 'static,
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    BIn: Body + Unpin + Send + 'static,
    <BIn as Body>::Data: Send,
    <BIn as Body>::Error: Into<BoxError>,
{
    type Response = Http2Connection<BIn>;

    type Error = hyper::Error;
    type Future = self::future::HttpProtocolFuture<Http2Connection<BIn>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: IO) -> Self::Future {
        let builder = std::mem::replace(self, self.clone());
        // let info = stream.info();
        // let span = tracing::info_span!("connection", version=?http::Version::HTTP_11, peer=%info.remote_addr());
        self::future::HttpProtocolFuture::new(async move {
            let (sender, conn) = builder.handshake(TokioIo::new(req)).await?;
            tokio::spawn(async {
                tracing::trace!("spawned h2 connection");
                if let Err(err) = conn.await {
                    if err.is_user() {
                        tracing::error!(err = format!("{err:#}"), "h2 connection driver error");
                    } else {
                        tracing::debug!(err = format!("{err:#}"), "h2 connection driver error");
                    }
                }
                tracing::trace!("finished h2 connection");
                // let shutdown = running.into_future();
                // tokio::select! {
                //     conn_error = conn => {
                //         if let Err(err) = conn_error {
                //             if err.is_user() {
                //                 tracing::error!(err = format!("{err:#}"), "h2 connection driver error");
                //             } else {
                //                 tracing::debug!(err = format!("{err:#}"), "h2 connection driver error");
                //             }
                //         }
                //         tracing::trace!("finished h2 connection");
                //     },
                //     _ = shutdown => {
                //         tracing::trace!("got shutdown signal in client");
                //     }
                // };
            });
            Ok(Http2Connection::new(sender))
        })
    }
}

mod future {
    use std::{
        fmt,
        future::Future,
        pin::Pin,
        task::{Context, Poll},
    };

    use crate::BoxFuture;

    pub struct HttpProtocolFuture<C> {
        inner: BoxFuture<'static, Result<C, hyper::Error>>,
    }

    impl<C> fmt::Debug for HttpProtocolFuture<C> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("HttpProtocolFuture").finish()
        }
    }

    impl<C> HttpProtocolFuture<C> {
        pub(super) fn new<F>(inner: F) -> Self
        where
            F: Future<Output = Result<C, hyper::Error>> + Send + 'static,
        {
            Self {
                inner: Box::pin(inner),
            }
        }
    }

    impl<C> Future for HttpProtocolFuture<C> {
        type Output = Result<C, hyper::Error>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            Pin::new(&mut self.inner).poll(cx)
        }
    }
}

#[cfg(all(test, feature = "stream", feature = "tls"))]
mod tests {

    use chateau::client::conn::connection::ConnectionExt as _;
    use chateau::client::pool::PoolableConnection;
    use futures_util::{TryFutureExt, stream::StreamExt as _};
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tracing_test::traced_test;

    use crate::bridge::rt::TokioExecutor;
    use crate::client::Error;
    use crate::client::conn::Protocol as _;
    use crate::client::conn::{Stream, protocol::HttpProtocol};
    use crate::service::HttpConnectionInfo as _;

    use super::*;

    async fn transport() -> Result<(Stream, Stream), BoxError> {
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

        Ok((tx.into(), rx.into()))
    }

    #[tokio::test]
    #[traced_test]
    async fn http_connector_request_h2() {
        use chateau::client::conn::Connection as _;

        let mut builder = Http2Builder::new(TokioExecutor::new());

        let (stream, rx) = transport().await.unwrap();

        let mut conn = builder.connect(stream).await.unwrap();
        conn.when_ready().await.unwrap();
        assert!(conn.is_open());
        assert_eq!(conn.version(), HttpProtocol::Http2);
        assert!(conn.can_share());
        assert!(conn.reuse().is_some());

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
            use tracing::trace;

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
