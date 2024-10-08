//! Duplex transport for patron clients

use std::io;
use std::task::{Context, Poll};

use crate::BoxFuture;
use http::Uri;

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

impl tower::Service<Uri> for DuplexTransport {
    type Response = Stream;

    type Error = io::Error;

    type Future = BoxFuture<'static, Result<Stream, io::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: Uri) -> Self::Future {
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
    use tower::ServiceExt;

    use super::*;
    use crate::info::DuplexAddr;
    use crate::info::HasConnectionInfo as _;
    use crate::server::conn::AcceptExt as _;

    #[tokio::test]
    async fn test_duplex_transport() {
        let (client, srv) = crate::stream::duplex::pair();

        let transport = DuplexTransport::new(1024, client);

        let (io, _) = tokio::join!(
            async {
                transport
                    .oneshot("https://example.com".parse().unwrap())
                    .await
                    .unwrap()
            },
            async { srv.accept().await.unwrap() }
        );
        let info = io.info();

        assert_eq!(info.local_addr, DuplexAddr::new());
        assert_eq!(info.remote_addr, DuplexAddr::new());
    }
}
