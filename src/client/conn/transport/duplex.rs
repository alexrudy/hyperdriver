//! Duplex transport for patron clients

use std::io;
use std::task::{Context, Poll};

use crate::BoxFuture;

use crate::stream::duplex::DuplexStream as Stream;

/// Transport via duplex stream
#[derive(Debug, Clone)]
pub struct DuplexTransport {
    max_buf_size: usize,
    client: crate::stream::duplex::DuplexClient,
}

impl DuplexTransport {
    /// Create a new `DuplexTransport`
    pub fn new(max_buf_size: usize, client: crate::stream::duplex::DuplexClient) -> Self {
        Self {
            max_buf_size,
            client,
        }
    }
}

impl tower::Service<http::request::Parts> for DuplexTransport {
    type Response = Stream;

    type Error = io::Error;

    type Future = BoxFuture<'static, Result<Stream, io::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: http::request::Parts) -> Self::Future {
        let client = self.client.clone();
        let max_buf_size = self.max_buf_size;
        let fut = async move {
            let stream = client.connect(max_buf_size).await?;
            Ok(stream)
        };

        Box::pin(fut)
    }
}

#[cfg(all(test, feature = "server"))]
mod tests {

    use hyper::client::conn::http1;

    use super::*;
    use crate::client::conn::connection::notify::Watcher;
    use crate::client::conn::protocol::info::ProtocolWithInfo;
    use crate::client::conn::protocol::notify::ProtocolWithNotify;
    use crate::client::conn::transport::TransportExt;
    use crate::client::pool::UriKey;
    use crate::info::ConnectionInfo;
    use crate::info::DuplexAddr;
    use crate::info::HasConnectionInfo as _;
    use crate::server::conn::AcceptExt as _;
    use crate::server::Protocol as _;
    use crate::service::RequestExecutor;
    use crate::Body;

    #[tokio::test]
    async fn duplex_transport() {
        let (client, srv) = crate::stream::duplex::pair();

        let transport = DuplexTransport::new(1024, client);

        let (io, _) = tokio::join!(
            async {
                TransportExt::oneshot(transport, "https://example.com")
                    .await
                    .unwrap()
            },
            async { srv.accept().await.unwrap() }
        );
        let info = io.info();

        assert_eq!(info.local_addr, info.remote_addr);
        assert_ne!(info.local_addr, DuplexAddr::new());
    }

    #[tokio::test]
    async fn duplex_pool() {
        let _ = tracing_subscriber::fmt::try_init();

        let (client, srv) = crate::stream::duplex::pair();
        let transport = DuplexTransport::new(1024, client);
        let protocol = http1::Builder::new();

        let svc: crate::client::ConnectionPoolService<_, _, _, Body, UriKey> =
            crate::client::ConnectionPoolService::new(
                transport,
                ProtocolWithNotify::new(ProtocolWithInfo::new(protocol)),
                RequestExecutor::new(),
                Default::default(),
            );

        let req = http::Request::get("http://wherever.what")
            .body(Body::from("hello world"))
            .unwrap();

        let echo = tower::service_fn(|req: http::Request<Body>| async {
            http::Response::builder().status(200).body(req.into_body())
        });

        tokio::join!(
            async move {
                let res = tower::ServiceExt::oneshot(svc.clone(), req).await.unwrap();
                let info: &ConnectionInfo<DuplexAddr> = res.extensions().get().unwrap();

                let mut watch = res.extensions().get::<Watcher>().unwrap().clone();
                watch.ready().await.unwrap();

                let req = http::Request::get("http://wherever.what")
                    .body(Body::from("hello world"))
                    .unwrap();

                // Check that this succeds and that the info is the same / and therefore
                // the connection is re-used.
                let res2 = tower::ServiceExt::oneshot(svc.clone(), req).await.unwrap();
                let info2: &ConnectionInfo<DuplexAddr> = res2.extensions().get().unwrap();
                assert_eq!(info, info2);
            },
            async {
                let conn = srv.accept().await.unwrap();
                let builder = hyper::server::conn::http1::Builder::new();
                builder
                    .serve_connection_with_upgrades(conn, echo)
                    .await
                    .unwrap();
            }
        );
    }
}
