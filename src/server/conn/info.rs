//! Tower middleware for collecting connection information after a handshake has been completed.
//!
//! This middleware applies to the request stack, but recieves the connection info from the acceptor stack.

use std::{fmt, task::Poll};

use hyper::{Request, Response};
use tower::{Layer, Service};

use crate::info::{ConnectionInfo, HasConnectionInfo};
use crate::service::ServiceRef;

/// A middleware which adds connection information to the request extensions.
///
/// This layer is meant to be applied to the "make service" part of the stack:
/// ```rust
/// # use std::convert::Infallible;
/// # use hyperdriver::Body;
/// # use hyperdriver::info::ConnectionInfo;
/// # use hyperdriver::server::conn::MakeServiceConnectionInfoLayer;
/// # use tower::Layer;
/// use hyperdriver::service::{make_service_fn, service_fn};
/// use tower::make::Shared;
///
/// # async fn make_service_with_layer() {
///
/// let service = service_fn(|req: http::Request<Body>| async move {
///    let info = req.extensions().get::<ConnectionInfo>().unwrap();
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
        future::MakeServiceConnectionInfoFuture::new(inner.call(stream), info)
    }
}

mod future {

    use pin_project::pin_project;
    use std::future::Future;

    use crate::service::ServiceRef;

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
    A: Clone + Send + Sync + 'static,
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
            req.extensions_mut().insert(info);
        } else {
            tracing::error!("Connection called twice, info is not available");
        }
        inner.call(req)
    }
}

#[cfg(test)]
mod tests {

    use std::convert::Infallible;

    use tower::{make::Shared, ServiceBuilder};

    use crate::{info::DuplexAddr, server::conn::AcceptExt as _};

    use super::*;

    #[tokio::test]
    async fn connection_info_from_service() {
        let service = tower::service_fn(|req: http::Request<crate::Body>| {
            let info = req
                .extensions()
                .get::<ConnectionInfo<DuplexAddr>>()
                .unwrap();
            assert_eq!(*info.remote_addr(), *info.local_addr());
            async { Ok::<_, Infallible>(Response::new(())) }
        });

        let mut make_service = ServiceBuilder::new()
            .layer(MakeServiceConnectionInfoLayer::new())
            .service(Shared::new(service));

        let (client, incoming) = crate::stream::duplex::pair();

        let (_, conn) = tokio::try_join!(client.connect(1024), incoming.accept()).unwrap();

        let req = http::Request::new(crate::Body::empty());
        let mut svc = tower::Service::call(&mut make_service, &conn)
            .await
            .unwrap();

        svc.call(req).await.unwrap();
    }
}
