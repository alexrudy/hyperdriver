#![allow(clippy::type_complexity)]

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;
use std::task::ready;

use http;
use hyper::body::Incoming;
use hyper::rt::bounds::Http2ServerConnExec;
use hyper::service::HttpService;
use ouroboros::self_referencing;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use super::ConnectionError;
use super::auto::Builder;
use super::auto::UpgradableConnection;
use crate::BoxError;
use crate::body::IncomingRequestService;
use crate::bridge::io::TokioIo;
use crate::bridge::service::TowerHyperService;

type Connection<'a, S, IO, BIn, BOut, E> = UpgradableConnection<
    'a,
    TokioIo<IO>,
    TowerHyperService<IncomingRequestService<S, BIn, BOut>>,
    E,
>;

type Adapt<S, BIn, BOut> = TowerHyperService<IncomingRequestService<S, BIn, BOut>>;

/// Connection that handles the self-referential relationship between
/// the protocol and the connection.
#[self_referencing]
pub struct Connecting<S, IO, BIn, BOut, E>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    BIn: From<hyper::body::Incoming>,
    BOut: http_body::Body,
    E: 'static,
{
    protocol: Builder<E>,

    #[borrows(protocol)]
    #[not_covariant]
    conn: Pin<Box<Connection<'this, S, IO, BIn, BOut, E>>>,
}

impl<S, IO, BIn, BOut, E> Connecting<S, IO, BIn, BOut, E>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    BIn: From<hyper::body::Incoming> + 'static,
    BOut: http_body::Body + 'static,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    pub(crate) fn build(protocol: Builder<E>, service: S, stream: IO) -> Self {
        Self::new(protocol, move |protocol| {
            Box::pin(protocol.serve_connection_with_upgrades(
                TokioIo::new(stream),
                TowerHyperService::new(IncomingRequestService::new(service)),
            ))
        })
    }
}

impl<S, IO, BIn, BOut, E> crate::server::Connection for Connecting<S, IO, BIn, BOut, E>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    BIn: http_body::Body + From<hyper::body::Incoming> + 'static,
    BOut: http_body::Body + Send + 'static,
    BOut::Data: Send + 'static,
    BOut::Error: Into<BoxError>,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    E: Http2ServerConnExec<<Adapt<S, BIn, BOut> as HttpService<Incoming>>::Future, BOut>,
{
    fn graceful_shutdown(mut self: Pin<&mut Self>) {
        self.with_conn_mut(|conn| conn.as_mut().graceful_shutdown())
    }
}

impl<S, IO, BIn, BOut, E> Future for Connecting<S, IO, BIn, BOut, E>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    BIn: http_body::Body + From<hyper::body::Incoming> + 'static,
    BOut: http_body::Body + Send + 'static,
    BOut::Data: Send + 'static,
    BOut::Error: Into<BoxError>,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    E: Http2ServerConnExec<<Adapt<S, BIn, BOut> as HttpService<Incoming>>::Future, BOut>,
{
    type Output = Result<(), ConnectionError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match ready!(self.with_conn_mut(|conn| conn.as_mut().poll(cx))) {
            Ok(()) => Poll::Ready(Ok(())),
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}
