//! Tower middleware for collecting TLS connection information after a handshake has been completed.
//!
//! This middleware applies to the request stack, but recieves the connection info from the acceptor stack.

use std::{fmt, task::Poll};

use futures_core::future::BoxFuture;
use hyper::{Request, Response};
use tower::{Layer, Service};
use tracing::Instrument;

use crate::{service::ServiceRef, stream::tls::TlsHandshakeInfo};

/// A middleware which adds TLS connection information to the request extensions.
#[derive(Debug, Clone, Default)]
pub struct TlsConnectionInfoLayer {
    _priv: (),
}

impl TlsConnectionInfoLayer {
    /// Create a new `TlsConnectionInfoLayer`.
    pub fn new() -> Self {
        Self { _priv: () }
    }
}

impl<S> Layer<S> for TlsConnectionInfoLayer {
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
    /// Create a new `TlsConnectionInfoService` wrapping `inner` service,
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S, IO> Service<&IO> for TlsConnectionInfoService<S>
where
    S: ServiceRef<IO> + Clone + Send + 'static,
    IO: TlsHandshakeInfo,
{
    type Response = TlsConnection<S::Response>;

    type Error = S::Error;

    type Future = future::TlsConnectionFuture<S, IO>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, stream: &IO) -> Self::Future {
        let mut inner = self.inner.clone();
        let rx = stream.recv();
        future::TlsConnectionFuture::new(inner.call(stream), rx)
    }
}

mod future {
    use std::{future::Future, task::Poll};

    use pin_project::pin_project;

    use crate::info::tls::TlsConnectionInfoReciever;
    use crate::service::ServiceRef;

    use super::TlsConnection;

    #[pin_project]
    #[derive(Debug)]
    pub struct TlsConnectionFuture<S, IO>
    where
        S: ServiceRef<IO>,
    {
        #[pin]
        inner: S::Future,

        _io: std::marker::PhantomData<fn(&IO) -> ()>,

        rx: TlsConnectionInfoReciever,
    }

    impl<S, IO> TlsConnectionFuture<S, IO>
    where
        S: ServiceRef<IO>,
    {
        pub(super) fn new(inner: S::Future, rx: TlsConnectionInfoReciever) -> Self {
            Self {
                inner,
                rx,
                _io: std::marker::PhantomData,
            }
        }
    }

    impl<S, IO> Future for TlsConnectionFuture<S, IO>
    where
        S: ServiceRef<IO>,
    {
        type Output = Result<TlsConnection<S::Response>, S::Error>;
        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> Poll<Self::Output> {
            let this = self.project();
            match this.inner.poll(cx) {
                Poll::Ready(Ok(res)) => Poll::Ready(Ok(TlsConnection {
                    inner: res,
                    rx: this.rx.clone(),
                })),
                Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
                Poll::Pending => Poll::Pending,
            }
        }
    }
}

/// Tower middleware for collecting TLS connection information after a handshake has been completed.
#[derive(Debug)]
pub struct TlsConnection<S> {
    inner: S,
    rx: crate::info::tls::TlsConnectionInfoReciever,
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

        let span = tracing::info_span!("TLS");

        let fut = async move {
            async {
                tracing::trace!("getting TLS Connection information (sent from the acceptor)");
                if let Some(info) = rx.recv().await {
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
