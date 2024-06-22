//! TLS connector for TCP transports.
//!
use std::sync::Arc;

use futures_core::future::BoxFuture;
use hyper::Uri;
use rustls::ClientConfig;
use tokio::net::TcpStream;
use tower::Service;

use super::ClientTlsStream;

/// A connector which can be used to create TLS streams from TCP connections.
///
/// This connector is a wrapper around `tokio_rustls::TlsConnector` and `hyper::client::HttpConnector`.
#[derive(Clone, Debug)]
pub struct TlsConnector<P> {
    protocol: P,
    tls: Arc<ClientConfig>,
}

impl<P> TlsConnector<P> {
    /// Create a new connector with the given TLS configuration.
    pub fn new(tls: Arc<ClientConfig>, protocol: P) -> Self {
        Self { protocol, tls }
    }
}

impl<P> Service<Uri> for TlsConnector<P>
where
    P: Service<Uri, Response = TcpStream> + Clone + Send + 'static,
    P::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    P::Future: Send + 'static,
{
    type Response = ClientTlsStream<TcpStream>;

    type Error = Box<dyn std::error::Error + Send + Sync>;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.protocol.poll_ready(cx).map_err(|err| err.into())
    }

    fn call(&mut self, req: Uri) -> Self::Future {
        let domain = req
            .host()
            .ok_or(rustls::pki_types::InvalidDnsNameError)
            .and_then(|host| host.to_owned().try_into());

        let tls = self.tls.clone();
        let conn = self.protocol.call(req);

        let fut = async move {
            let stream = conn.await.map_err(Into::into)?;
            let connect = tokio_rustls::TlsConnector::from(tls).connect(domain?, stream);
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(ClientTlsStream::from(connect))
        };
        Box::pin(fut)
    }
}
