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

use crate::bridge::io::TokioIo;
use crate::BoxError;
use chateau::info::HasConnectionInfo;

pub mod auto;
#[cfg(feature = "mocks")]
pub mod mock;
pub use hyper::client::conn::http1;
pub use hyper::client::conn::http2;

use super::connection::Http1Connection;

/// The HTTP protocol to use for a connection.
///
/// This differs from the HTTP version in that it is constrained to the two flavors of HTTP
/// protocol, HTTP/1.1 and HTTP/2. HTTP/3 is not yet supported. HTTP/0.9 and HTTP/1.0 are
/// supported by HTTP/1.1.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum HttpProtocol {
    /// Connect using HTTP/1.1
    Http1,

    /// Connect using HTTP/2
    Http2,
}

impl HttpProtocol {
    /// Does this protocol allow multiplexing?
    pub fn multiplex(&self) -> bool {
        matches!(self, Self::Http2)
    }

    /// HTTP Version
    ///
    /// Convert the protocol to an HTTP version.
    ///
    /// For HTTP/1.1, this returns `::http::Version::HTTP_11`.
    /// For HTTP/2, this returns `::http::Version::HTTP_2`.
    pub fn version(&self) -> ::http::Version {
        match self {
            Self::Http1 => ::http::Version::HTTP_11,
            Self::Http2 => ::http::Version::HTTP_2,
        }
    }
}

impl From<::http::Version> for HttpProtocol {
    fn from(version: ::http::Version) -> Self {
        match version {
            ::http::Version::HTTP_11 | ::http::Version::HTTP_10 => Self::Http1,
            ::http::Version::HTTP_2 => Self::Http2,
            _ => panic!("Unsupported HTTP protocol"),
        }
    }
}

pub struct Http1Builder<B>(hyper::client::conn::http1::Builder, PhantomData<fn(B)>);

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
            Ok(sender)
        })
    }
}

pub struct Http2Builder<B, E>(hyper::client::conn::http2::Builder<E>, PhantomData<fn(B)>);

impl<B, E> Deref for Http2Builder<B, E> {
    type Target = hyper::client::conn::http2::Builder<E>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<B, E> DerefMut for Http2Builder<B, E> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
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
    type Response = hyper::client::conn::http2::SendRequest<BIn>;

    type Error = hyper::Error;
    type Future = self::future::HttpProtocolFuture<hyper::client::conn::http2::SendRequest<BIn>>;

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
                if let Err(err) = conn.await {
                    if err.is_user() {
                        tracing::error!(err = format!("{err:#}"), "h2 connection driver error");
                    } else {
                        tracing::debug!(err = format!("{err:#}"), "h2 connection driver error");
                    }
                }
            });
            Ok(sender)
        })
    }
}

mod future {
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Poll},
    };

    use crate::BoxFuture;

    pub struct HttpProtocolFuture<C> {
        inner: BoxFuture<'static, Result<C, hyper::Error>>,
    }

    impl<C> HttpProtocolFuture<C> {
        pub(super) fn new<F>(inner: F) -> Self
        where
            F: Future<Output = Result<C, hyper::Error>> + 'static,
        {
            Self {
                inner: Box::pin(inner),
            }
        }
    }

    impl<C> Future for HttpProtocolFuture<C> {
        type Output = Result<C, hyper::Error>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.inner.poll_unpin(cx)
        }
    }
}
