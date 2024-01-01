//! Tower middleware for collecting TLS connection information after a handshake has been completed.
//!
//! This middleware applies to the request stack, but recieves the connection info from the acceptor stack.

use std::{
    convert::Infallible,
    fmt,
    future::{ready, Ready},
    task::Poll,
};

use futures_core::future::BoxFuture;
use hyper::{Request, Response};
use tower::{Layer, Service};
use tracing::{dispatcher, Instrument};

use super::acceptor::TlsStream;

pub struct TlsConnectLayer;

impl<S> Layer<S> for TlsConnectLayer {
    type Service = TlsConnectionInfoService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TlsConnectionInfoService::new(inner)
    }
}

/// Tower middleware to set up TLS connection information after a handshake has been completed on initial TLS stream.
#[derive(Debug, Clone)]
pub struct TlsConnectionInfoService<S> {
    inner: S,
}

impl<S> TlsConnectionInfoService<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S, IO> Service<&TlsStream<IO>> for TlsConnectionInfoService<S>
where
    S: Clone + Send + 'static,
{
    type Response = TlsConnection<S>;

    type Error = Infallible;

    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, stream: &TlsStream<IO>) -> Self::Future {
        let inner = self.inner.clone();
        let rx = stream.rx.clone();
        ready(Ok(TlsConnection { inner, rx }))
    }
}

/// Tower middleware for collecting TLS connection information after a handshake has been completed.
pub struct TlsConnection<S> {
    inner: S,
    rx: crate::tls::info::TlsConnectionInfoReciever,
}

impl<S, BIn, BOut> Service<Request<BIn>> for TlsConnection<S>
where
    S: Service<Request<BIn>, Response = Response<BOut>> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: fmt::Display,
    BIn: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<BIn>) -> Self::Future {
        let rx = self.rx.clone();
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);

        let span = tracing::info_span!("TLS Connection");
        dispatcher::get_default(|dispatch| {
            let id = span.id().expect("Missing ID; this is a bug");
            if let Some(current) = dispatch.current_span().id() {
                dispatch.record_follows_from(&id, current)
            }
        });

        let fut = async move {
            async {
                tracing::trace!("getting TLS Connection information (sent from the acceptor)");
                if let Ok(info) = rx.recv().await {
                    req.extensions_mut().insert(info);
                }
            }
            .instrument(span.clone())
            .await;
            inner.call(req).instrument(span).await
        };

        Box::pin(fut)
    }
}
