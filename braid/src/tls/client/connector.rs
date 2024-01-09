//! TLS connector for TCP transports.
//!
use std::sync::Arc;

use futures_core::future::BoxFuture;
use hyper::Uri;
use hyper_util::client::legacy::connect::HttpConnector;
use rustls::ClientConfig;
use tokio::net::TcpStream;
use tower::Service;

use super::TlsStream;

/// A connector which can be used to create TLS streams from TCP connections.
///
/// This connector is a wrapper around `tokio_rustls::TlsConnector` and `hyper::client::HttpConnector`.
#[derive(Clone, Debug)]
pub struct TlsConnector {
    http: HttpConnector,
    tls: Arc<ClientConfig>,
}

impl TlsConnector {
    /// Create a new connector with the given TLS configuration.
    pub fn new(tls: Arc<ClientConfig>) -> Self {
        Self {
            http: HttpConnector::new(),
            tls,
        }
    }

    /// Set the http connector to use for TCP connections.
    pub fn with_http(self, mut http: HttpConnector) -> Self {
        http.enforce_http(false);
        Self { http, ..self }
    }
}

impl Service<Uri> for TlsConnector {
    type Response = TlsStream<TcpStream>;

    type Error = Box<dyn std::error::Error + Send + Sync>;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.http.poll_ready(cx).map_err(|err| err.into())
    }

    fn call(&mut self, req: Uri) -> Self::Future {
        let domain = req
            .host()
            .ok_or(rustls::pki_types::InvalidDnsNameError)
            .and_then(|host| host.to_owned().try_into());

        let tls = self.tls.clone();
        let conn = self.http.call(req);

        let fut = async move {
            let stream = conn.await?;
            let connect =
                tokio_rustls::TlsConnector::from(tls).connect(domain?, stream.into_inner());
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(TlsStream::from(connect))
        };
        Box::pin(fut)
    }
}
