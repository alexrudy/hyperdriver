//! Tower middleware for collecting TLS connection information after a handshake has been completed.
//!
//! This middleware applies to the request stack, but recieves the connection info from the acceptor stack.

use std::{fmt, task::Poll};

use crate::BoxFuture;
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
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, stream: &IO) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);
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
#[derive(Debug, Clone)]
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
                    tracing::trace!(?info, "TLS Connection information received");
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::convert::Infallible;

    use tower::make::Shared;
    use tower::Service;

    use crate::fixtures;

    use crate::client::conn::transport::duplex::DuplexTransport;
    use crate::client::conn::transport::TransportExt as _;
    use crate::client::conn::Transport as _;

    use crate::info::TlsConnectionInfo;
    use crate::server::conn::AcceptExt as _;
    use crate::stream::tls::TlsHandshakeStream as _;

    #[tokio::test]
    async fn tls_server_info() {
        let _ = tracing_subscriber::fmt::try_init();
        let _ = color_eyre::install();
        fixtures::tls_install_default();

        let _guard = tracing::info_span!("tls").entered();

        let service = tower::service_fn(|req: http::Request<crate::Body>| async {
            req.extensions().get::<TlsConnectionInfo>().unwrap();
            drop(req);
            Ok::<_, Infallible>(Response::new(crate::Body::empty()))
        });

        let (client, incoming) = crate::stream::duplex::pair();

        let acceptor = crate::server::conn::Acceptor::from(incoming)
            .with_tls(crate::fixtures::tls_server_config().into());

        let mut client = DuplexTransport::new(1024, client)
            .with_tls(crate::fixtures::tls_client_config().into());

        let client = async move {
            let mut stream = client
                .connect("https://example.com".parse().unwrap())
                .await
                .unwrap();

            tracing::debug!("client connected");

            stream.finish_handshake().await.unwrap();

            tracing::debug!("client handshake finished");

            stream
        }
        .instrument(tracing::info_span!("client"));

        let server = async move {
            let mut conn = acceptor.accept().await.unwrap();

            tracing::debug!("server accepted");

            let mut make_service = TlsConnectionInfoLayer::new().layer(Shared::new(service));

            conn.finish_handshake().await.unwrap();
            tracing::debug!("server handshake finished");

            let mut svc = Service::call(&mut make_service, &conn).await.unwrap();
            tracing::debug!("server created");

            let _ = tower::Service::call(&mut svc, http::Request::new(crate::Body::empty()))
                .await
                .unwrap();

            tracing::debug!("server request handled");
            conn
        }
        .instrument(tracing::info_span!("server"));

        let (stream, conn) = tokio::join!(client, server);
        drop((stream, conn));
    }
}
