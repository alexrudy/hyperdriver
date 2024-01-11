use std::task::{Context, Poll};

use arnold::Body;
use camino::Utf8Path;
use futures_util::{future::BoxFuture, TryFutureExt};
use hyper::{body::Incoming, Uri};
use thiserror::Error;

pub(crate) fn find_service_name(uri: &Uri) -> Option<&str> {
    let path = Utf8Path::new(uri.path());
    path.components().nth(1).map(|c| c.as_str())
}

#[derive(Debug, Error)]
pub enum RouterError {
    #[error("invalid uri")]
    InvalidUri,

    #[error("service '{0}' not found")]
    ServiceNotFound(String),

    #[error("client: {0}")]
    Client(#[from] patron::Error),
}

/// A router to multiplex GRPC services
#[derive(Debug, Clone)]
pub struct GrpcRouter {
    registry: crate::ServiceRegistry,
    client: crate::Client,
}

impl GrpcRouter {
    /// Create a new GRPC Router.
    pub(crate) fn new(registry: crate::ServiceRegistry) -> Self {
        let client = registry.client();
        Self { registry, client }
    }
}

impl tower::Service<http::Request<Body>> for GrpcRouter {
    type Response = http::Response<Incoming>;
    type Error = RouterError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut req: http::Request<Body>) -> Self::Future {
        let service = match find_service_name(req.uri()) {
            Some(service) => service,
            None => {
                tracing::debug!(uri=%req.uri(), "no service found in URI");

                return Box::pin(futures_util::future::ready(Err(RouterError::InvalidUri)));
            }
        };

        let name = match self.registry.inner.get_name(service) {
            Some(name) => name,
            None => {
                tracing::debug!(uri=%req.uri(), "service {service} not found");

                return Box::pin(futures_util::future::ready(Err(
                    RouterError::ServiceNotFound(service.into()),
                )));
            }
        };

        let uri = req.uri_mut();
        let mut parts = uri.clone().into_parts();
        parts.scheme = Some("svc".parse().unwrap());
        parts.authority = Some(name.parse().expect("service names are valid authorities"));
        *uri = Uri::from_parts(parts).expect("rebuilt a valid URI");

        tracing::debug!("Upstream uri: {uri}");

        Box::pin(self.client.call(req).map_err(|error| error.into()))
    }
}
