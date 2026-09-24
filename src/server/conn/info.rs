//! Tower middleware for collecting connection information after a handshake has been completed.
//!
//! This middleware applies to the request stack, but recieves the connection info from the acceptor stack.

use std::{fmt, task::Poll};

use hyper::{Request, Response};
use tower::{Layer, Service};

use chateau::info::{Address, ConnectionInfo, HasConnectionInfo};
use chateau::services::ServiceRef;

#[cfg(feature = "tls")]
pub use tls::{
    ConnectionWithTlsInfo, MakeServiceTlsConnectionInfoLayer, MakeServiceTlsConnectionInfoService,
};

/// A middleware which adds connection information to the request extensions.
///
/// This layer is meant to be applied to the "make service" part of the stack:
/// ```rust
/// # use std::convert::Infallible;
/// # use hyperdriver::Body;
/// # use hyperdriver::info::ConnectionInfo;
/// # use hyperdriver::server::conn::MakeServiceConnectionInfoLayer;
/// # use tower::Layer;
/// # use std::net::SocketAddr;
/// # use tower::service_fn;
/// use tower::make::Shared;
///
/// # async fn make_service_with_layer() {
///
/// let service = service_fn(|req: http::Request<Body>| async move {
///    let info = req.extensions().get::<ConnectionInfo<SocketAddr>>().unwrap();
///    println!("Connection info: {:?}", info);
///    Ok::<_, Infallible>(http::Response::new(Body::from("Hello, World!")))
/// });
///
/// let make_service = MakeServiceConnectionInfoLayer::default().layer(Shared::new(service));
/// # }
///
///
#[derive(Clone, Default)]
pub struct MakeServiceConnectionInfoLayer {
    _priv: (),
}

impl MakeServiceConnectionInfoLayer {
    /// Create a new `MakeServiceConnectionInfoLayer`.
    pub fn new() -> Self {
        Self { _priv: () }
    }
}

impl fmt::Debug for MakeServiceConnectionInfoLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MakeServiceConnectionInfoLayer").finish()
    }
}

impl<S> Layer<S> for MakeServiceConnectionInfoLayer {
    type Service = MakeServiceConnectionInfoService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MakeServiceConnectionInfoService::new(inner)
    }
}

/// A service which adds connection information to the request extensions.
///
/// This is applied to the "make service" part of the stack.
///
/// See [`MakeServiceConnectionInfoLayer`] for more details.
#[derive(Debug, Clone)]
pub struct MakeServiceConnectionInfoService<C> {
    inner: C,
}

impl<C> MakeServiceConnectionInfoService<C> {
    /// Create a new `StartConnectionInfoService` wrapping `inner` service,
    /// and applying `info` to the request extensions.
    pub fn new(inner: C) -> Self {
        Self { inner }
    }
}

impl<C, IO> Service<&IO> for MakeServiceConnectionInfoService<C>
where
    C: ServiceRef<IO> + Clone + Send + 'static,
    IO: HasConnectionInfo + Send + 'static,
    IO::Addr: Clone + Send + Sync + 'static,
{
    type Response = ConnectionWithInfo<C::Response, IO::Addr>;

    type Error = C::Error;

    type Future = future::MakeServiceConnectionInfoFuture<C, IO>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, stream: &IO) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);
        let info = stream.info();
        tracing::trace!("prepared connection info from stream");
        future::MakeServiceConnectionInfoFuture::new(inner.call(stream), info)
    }
}

mod future {

    use pin_project::pin_project;
    use std::future::Future;

    use chateau::services::ServiceRef;

    use super::*;

    #[pin_project]
    #[derive(Debug)]
    pub struct MakeServiceConnectionInfoFuture<S, IO>
    where
        S: ServiceRef<IO>,
        IO: HasConnectionInfo,
    {
        #[pin]
        inner: S::Future,
        info: Option<ConnectionInfo<IO::Addr>>,
    }

    impl<S, IO> MakeServiceConnectionInfoFuture<S, IO>
    where
        S: ServiceRef<IO>,
        IO: HasConnectionInfo,
    {
        pub(super) fn new(inner: S::Future, info: ConnectionInfo<IO::Addr>) -> Self {
            Self {
                inner,
                info: Some(info),
            }
        }
    }

    impl<S, IO> Future for MakeServiceConnectionInfoFuture<S, IO>
    where
        S: ServiceRef<IO>,
        IO: HasConnectionInfo,
    {
        type Output = Result<ConnectionWithInfo<S::Response, IO::Addr>, S::Error>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> Poll<Self::Output> {
            let this = self.project();

            match this.inner.poll(cx) {
                Poll::Ready(Ok(inner)) => Poll::Ready(Ok(ConnectionWithInfo {
                    inner,
                    info: this.info.take(),
                })),
                Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                Poll::Pending => Poll::Pending,
            }
        }
    }
}

/// Interior service which adds connection information to the request extensions.
///
/// This service wraps the request/response service, not the connector service.
#[derive(Debug, Clone)]
pub struct ConnectionWithInfo<S, A> {
    inner: S,
    info: Option<ConnectionInfo<A>>,
}

impl<S, A, BIn, BOut> Service<Request<BIn>> for ConnectionWithInfo<S, A>
where
    S: Service<Request<BIn>, Response = Response<BOut>> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: fmt::Display,
    BIn: Send + 'static,
    A: Address + Send + Sync + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<BIn>) -> Self::Future {
        let next = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, next);

        if let Some(info) = self.info.take() {
            tracing::trace!(
                "Inserting connection info {}",
                std::any::type_name_of_val(&info),
            );
            req.extensions_mut().insert(info.erase());
        } else {
            tracing::error!("Connection called twice, info is not available");
        }
        inner.call(req)
    }
}

/// Tower middleware for collecting TLS connection information after a
/// handshake has been completed, and attaching it to request extensions.
///
/// This is analogous to [`MakeServiceConnectionInfoLayer`] and
/// [`ConnectionWithInfo`], but for [`chateau::info::TlsConnectionInfo`].
///
/// It exists because `chateau::server::conn::tls::info::TlsConnectionInfoLayer`
/// is generic over the request type, and so has no way to insert the TLS
/// connection information into request extensions. This layer is specialized
/// to `hyper::Request`/`hyper::Response`, so it can do exactly that.
#[cfg(feature = "tls")]
mod tls {
    use std::{fmt, task::Poll};

    use hyper::{Request, Response};
    use tower::{Layer, Service};

    use chateau::info::tls::TlsConnectionInfoReceiver;
    use chateau::services::ServiceRef;
    use chateau::stream::tls::TlsHandshakeInfo;

    use crate::BoxFuture;

    /// A middleware which adds TLS connection information to the request extensions.
    ///
    /// This layer is meant to be applied to the "make service" part of the stack, after
    /// the request/response service has already been established (for example, via
    /// [`crate::server::ServerConnectionInfoExt::with_tls_connection_info`]).
    #[derive(Clone, Default)]
    pub struct MakeServiceTlsConnectionInfoLayer {
        _priv: (),
    }

    impl MakeServiceTlsConnectionInfoLayer {
        /// Create a new `MakeServiceTlsConnectionInfoLayer`.
        pub fn new() -> Self {
            Self { _priv: () }
        }
    }

    impl fmt::Debug for MakeServiceTlsConnectionInfoLayer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("MakeServiceTlsConnectionInfoLayer").finish()
        }
    }

    impl<S> Layer<S> for MakeServiceTlsConnectionInfoLayer {
        type Service = MakeServiceTlsConnectionInfoService<S>;

        fn layer(&self, inner: S) -> Self::Service {
            MakeServiceTlsConnectionInfoService::new(inner)
        }
    }

    /// A service which adds TLS connection information to the request extensions.
    ///
    /// This is applied to the "make service" part of the stack.
    ///
    /// See [`MakeServiceTlsConnectionInfoLayer`] for more details.
    #[derive(Debug, Clone)]
    pub struct MakeServiceTlsConnectionInfoService<C> {
        inner: C,
    }

    impl<C> MakeServiceTlsConnectionInfoService<C> {
        /// Create a new `MakeServiceTlsConnectionInfoService` wrapping `inner` service.
        pub fn new(inner: C) -> Self {
            Self { inner }
        }
    }

    impl<C, IO> Service<&IO> for MakeServiceTlsConnectionInfoService<C>
    where
        C: ServiceRef<IO> + Clone + Send + 'static,
        IO: TlsHandshakeInfo,
    {
        type Response = ConnectionWithTlsInfo<C::Response>;

        type Error = C::Error;

        type Future = future::MakeServiceTlsConnectionInfoFuture<C, IO>;

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
            tracing::trace!("captured TLS connection info receiver from stream");
            future::MakeServiceTlsConnectionInfoFuture::new(inner.call(stream), rx)
        }
    }

    mod future {
        use std::{future::Future, task::Poll};

        use pin_project::pin_project;

        use chateau::info::tls::TlsConnectionInfoReceiver;
        use chateau::services::ServiceRef;

        use super::ConnectionWithTlsInfo;

        #[pin_project]
        #[derive(Debug)]
        pub struct MakeServiceTlsConnectionInfoFuture<S, IO>
        where
            S: ServiceRef<IO>,
        {
            #[pin]
            inner: S::Future,
            rx: Option<TlsConnectionInfoReceiver>,
        }

        impl<S, IO> MakeServiceTlsConnectionInfoFuture<S, IO>
        where
            S: ServiceRef<IO>,
        {
            pub(super) fn new(inner: S::Future, rx: TlsConnectionInfoReceiver) -> Self {
                Self {
                    inner,
                    rx: Some(rx),
                }
            }
        }

        impl<S, IO> Future for MakeServiceTlsConnectionInfoFuture<S, IO>
        where
            S: ServiceRef<IO>,
        {
            type Output = Result<ConnectionWithTlsInfo<S::Response>, S::Error>;

            fn poll(
                self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
            ) -> Poll<Self::Output> {
                let this = self.project();

                match this.inner.poll(cx) {
                    Poll::Ready(Ok(inner)) => Poll::Ready(Ok(ConnectionWithTlsInfo {
                        inner,
                        rx: this.rx.take().expect("future polled after completion"),
                    })),
                    Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }

    /// Interior service which adds TLS connection information to the request extensions.
    ///
    /// This service wraps the request/response service, not the connector service.
    #[derive(Debug, Clone)]
    pub struct ConnectionWithTlsInfo<S> {
        inner: S,
        rx: TlsConnectionInfoReceiver,
    }

    impl<S, BIn, BOut> Service<Request<BIn>> for ConnectionWithTlsInfo<S>
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
            let next = self.inner.clone();
            let mut inner = std::mem::replace(&mut self.inner, next);

            Box::pin(async move {
                match rx.recv().await {
                    Some(info) => {
                        tracing::trace!(?info, "inserting TLS connection info");
                        req.extensions_mut().insert(info);
                    }
                    None => {
                        tracing::trace!(
                            "no TLS connection info available for this request (non-TLS connection?)"
                        );
                    }
                }
                inner.call(req).await
            })
        }
    }
}

#[cfg(test)]
mod tests {

    use std::convert::Infallible;

    use tower::{ServiceBuilder, make::Shared};

    use crate::{info::DuplexAddr, server::conn::AcceptExt as _};

    use super::*;

    #[tokio::test]
    async fn connection_info_from_service() {
        let service = tower::service_fn(|req: http::Request<crate::Body>| {
            let info = req.extensions().get::<ConnectionInfo>().unwrap();
            assert_eq!(
                info.remote_addr_as::<DuplexAddr>(),
                Some(&DuplexAddr::new())
            );
            async { Ok::<_, Infallible>(Response::new(())) }
        });

        let mut make_service = ServiceBuilder::new()
            .layer(MakeServiceConnectionInfoLayer::new())
            .service(Shared::new(service));

        let (client, incoming) = chateau::stream::duplex::pair();

        let (_, conn) = tokio::try_join!(client.connect(1024), incoming.accept()).unwrap();

        let req = http::Request::new(crate::Body::empty());
        let mut svc = tower::Service::call(&mut make_service, &conn)
            .await
            .unwrap();

        svc.call(req).await.unwrap();
    }

    #[cfg(all(feature = "tls", feature = "client", feature = "stream"))]
    #[tokio::test]
    async fn tls_connection_info_from_service() {
        use chateau::client::conn::Transport as _;
        use chateau::client::conn::transport::duplex::DuplexTransport;
        use chateau::info::TlsConnectionInfo;
        use chateau::stream::tls::TlsHandshakeStream as _;

        use crate::client::conn::HttpTlsTransport;

        crate::fixtures::tls_install_default();

        let service = tower::service_fn(|req: http::Request<crate::Body>| {
            let info = req.extensions().get::<TlsConnectionInfo>().unwrap();
            assert!(matches!(
                info.alpn.as_deref(),
                Some("h2") | Some("http/1.1")
            ));
            async { Ok::<_, Infallible>(Response::new(())) }
        });

        let mut make_service = ServiceBuilder::new()
            .layer(MakeServiceTlsConnectionInfoLayer::new())
            .service(Shared::new(service));

        let (client, incoming) = chateau::stream::duplex::pair();

        let acceptor = crate::server::conn::Acceptor::from(incoming)
            .with_tls(crate::fixtures::tls_server_config().into());

        let mut transport = HttpTlsTransport::new(
            DuplexTransport::new(1024, client),
            crate::fixtures::tls_client_config().into(),
        );

        let req = http::Request::get("https://example.com").body(()).unwrap();

        let client_task = async move {
            let mut stream = transport.connect(&req).await.unwrap();
            stream.finish_handshake().await.unwrap();
            stream
        };

        let server_task = async move {
            let mut conn = acceptor.accept().await.unwrap();

            let mut svc = tower::Service::call(&mut make_service, &conn)
                .await
                .unwrap();

            conn.finish_handshake().await.unwrap();

            let req = http::Request::new(crate::Body::empty());
            svc.call(req).await.unwrap();
            conn
        };

        let (stream, conn) = tokio::join!(client_task, server_task);
        drop((stream, conn));
    }
}
