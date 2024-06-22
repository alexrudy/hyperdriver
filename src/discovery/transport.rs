use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use camino::Utf8Path;
use futures_util::future::BoxFuture;
use futures_util::FutureExt as _;
use hyper::Uri;

use super::ConnectionError;
use super::ServiceRegistry;
use crate::client::stream::Stream as ClientStream;
use crate::client::TransportStream;

pub use builder::TransportBuilder;

/// A trait for extracting the service name from a request.
pub trait Scheme {
    /// The scheme used by the transport.
    fn scheme(&self) -> Cow<'_, str>;

    /// The service name extracted from the request.
    fn service<'u>(&self, uri: &'u Uri) -> Option<&'u str>;
}

type BoxScheme = Box<dyn Scheme + Sync + Send + 'static>;

/// A connection transport which uses roomservice's internal service registry.
#[derive(Clone)]
pub struct RegistryTransport {
    registry: ServiceRegistry,
    schemes: Arc<BTreeMap<String, BoxScheme>>,
}

struct SchemesDebug<'a, S>(&'a BTreeMap<String, S>);

impl<'a, S> fmt::Debug for SchemesDebug<'a, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut dbg = f.debug_list();
        for scheme in self.0.keys() {
            dbg.entry(&scheme);
        }
        dbg.finish()
    }
}

impl fmt::Debug for RegistryTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut dbg = f.debug_struct("RegistryTransport");
        dbg.field("registry", &self.registry);
        dbg.field("schemes", &SchemesDebug(&self.schemes)).finish()
    }
}

impl RegistryTransport {
    /// Create a new `RegistryTransport` builder with the given service registry.
    ///
    /// The builder can be used to add custom schemes to the transport.
    pub fn builder(registry: ServiceRegistry) -> builder::TransportBuilder {
        builder::TransportBuilder {
            registry,
            schemes: BTreeMap::new(),
        }
    }

    /// Create a new `RegistryTransport` with the given service registry and default schemes.
    ///
    /// The default schemes are `svc` and `grpc`, which follow the conventions of the svc protocol (using the host to
    /// determine the service) and the gRPC protocol (using the first path component to determine the service).
    pub fn with_default_schemes(registry: ServiceRegistry) -> Self {
        Self::builder(registry).add_default_schemes().build()
    }

    /// The inner service registry
    pub fn registry(&self) -> &ServiceRegistry {
        &self.registry
    }
}

impl tower::Service<Uri> for RegistryTransport {
    type Response = TransportStream<ClientStream>;

    type Error = ConnectionError;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Uri) -> Self::Future {
        match req.scheme_str().and_then(|scheme| self.schemes.get(scheme)) {
            Some(scheme) => {
                let registry = self.registry.clone();
                let service = scheme.service(&req).map(|s| s.to_owned());
                if let Some(service) = service {
                    (async move {
                        let stream = registry.connect(&service).await?;
                        TransportStream::new_stream(stream).await.map_err(|error| {
                            ConnectionError::Handshake {
                                error,
                                name: service,
                            }
                        })
                    })
                    .boxed()
                } else {
                    futures_util::future::ready(Err(ConnectionError::InvalidUri(req))).boxed()
                }
            }
            None => futures_util::future::ready(Err(ConnectionError::InvalidUri(req))).boxed(),
        }
    }
}

mod builder {
    use std::collections::BTreeMap;
    use std::fmt;
    use std::sync::Arc;

    use super::BoxScheme;
    use super::RegistryTransport;
    use super::Scheme;
    use super::SchemesDebug;
    use crate::discovery::ServiceRegistry;

    /// A builder for creating a `RegistryTransport`, by adding custom schemes.
    ///
    /// Each scheme is a way to determine the service name from a request URI.
    pub struct TransportBuilder {
        pub(crate) registry: ServiceRegistry,
        pub(crate) schemes: BTreeMap<String, BoxScheme>,
    }

    impl TransportBuilder {
        /// Add a custom scheme to the transport.
        pub fn add_scheme<S>(mut self, scheme: S) -> Self
        where
            S: Scheme + Send + Sync + 'static,
        {
            self.schemes
                .insert(scheme.scheme().into(), Box::new(scheme));
            self
        }

        /// Add the default `svc` and `grpc` schemes to the transport.
        pub fn add_default_schemes(self) -> Self {
            self.add_scheme(super::SvcScheme::default())
                .add_scheme(super::GrpcScheme::default())
        }

        /// Build the `RegistryTransport` with the given service registry and schemes.
        pub fn build(self) -> RegistryTransport {
            RegistryTransport {
                registry: self.registry,
                schemes: Arc::new(self.schemes),
            }
        }
    }

    impl fmt::Debug for TransportBuilder {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let mut dbg = f.debug_struct("TransportBuilder");
            dbg.field("registry", &self.registry);
            dbg.field("schemes", &SchemesDebug(&self.schemes)).finish()
        }
    }
}

/// A scheme for the `svc` protocol, which uses the host to determine which service to call.
#[derive(Debug)]
pub struct SvcScheme(String);

impl SvcScheme {
    /// Create a new `SvcScheme` with the given scheme.
    pub fn new<S>(scheme: S) -> Self
    where
        S: Into<String>,
    {
        SvcScheme(scheme.into())
    }
}

impl Default for SvcScheme {
    fn default() -> Self {
        SvcScheme("svc".into())
    }
}

impl Scheme for SvcScheme {
    fn scheme(&self) -> Cow<'_, str> {
        self.0.clone().into()
    }

    fn service<'u>(&self, uri: &'u Uri) -> Option<&'u str> {
        uri.host()
    }
}

/// A scheme for the `grpc` protocol, which uses the first path component to determine which service to call.
///
/// This is how the gRPC protocol works natively.
#[derive(Debug)]
pub struct GrpcScheme(String);

impl GrpcScheme {
    /// Create a new `GrpcScheme` with the given scheme.
    pub fn new<S>(scheme: S) -> Self
    where
        S: Into<String>,
    {
        GrpcScheme(scheme.into())
    }
}

impl Default for GrpcScheme {
    fn default() -> Self {
        GrpcScheme("grpc".into())
    }
}

impl Scheme for GrpcScheme {
    fn scheme(&self) -> Cow<'_, str> {
        self.0.clone().into()
    }

    fn service<'u>(&self, uri: &'u Uri) -> Option<&'u str> {
        let path = Utf8Path::new(uri.path());
        path.components().nth(1).map(|c| c.as_str())
    }
}

#[cfg(test)]
mod tests {
    use tower::{make::Shared, Service, ServiceExt};

    use crate::info::HasConnectionInfo;
    use crate::{body::Body, info::BraidAddr};

    use super::*;

    type BoxError = Box<dyn std::error::Error + Sync + std::marker::Send + 'static>;

    #[test]
    fn test_svc_scheme() {
        let scheme = SvcScheme::default();
        let uri = "svc://service".parse().unwrap();
        assert_eq!(scheme.service(&uri), Some("service"));
    }

    #[test]
    fn test_grpc_scheme() {
        let scheme = GrpcScheme::default();
        let uri = "grpc://host/service/method".parse().unwrap();
        assert_eq!(scheme.service(&uri), Some("service"));
    }

    #[derive(Debug, Clone)]
    struct Svc;

    impl Service<http::Request<Body>> for Svc {
        type Response = http::Response<Body>;
        type Error = BoxError;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: http::Request<Body>) -> Self::Future {
            let res = http::Response::new(Body::empty());
            std::future::ready(Ok(res))
        }
    }

    #[tokio::test]
    async fn test_transport() {
        let registry = ServiceRegistry::new();
        let svc = Svc;

        let server = registry.server(Shared::new(svc), "service").await.unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            server
                .with_graceful_shutdown(async move { rx.await.unwrap() })
                .await
                .unwrap();
        });

        let transport = RegistryTransport::with_default_schemes(registry);

        let uri = "svc://service".parse().unwrap();
        let stream = transport.oneshot(uri).await.unwrap();

        let info = stream.info();
        assert_eq!(info.remote_addr(), &BraidAddr::Duplex);

        tx.send(()).unwrap();
    }
}
