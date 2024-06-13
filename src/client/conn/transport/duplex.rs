//! Duplex transport for patron clients

use std::io;
use std::task::{Context, Poll};

use futures_util::future::BoxFuture;
use http::Uri;

use super::TransportStream;
use crate::stream::client::Stream;
use crate::stream::info::Protocol;

/// Transport via duplex stream
#[derive(Debug, Clone)]
pub struct DuplexTransport {
    max_buf_size: usize,
    protocol: Option<Protocol>,
    client: crate::stream::duplex::DuplexClient,
}

impl DuplexTransport {
    /// Create a new `DuplexTransport`
    pub fn new(
        max_buf_size: usize,
        protocol: Option<Protocol>,
        client: crate::stream::duplex::DuplexClient,
    ) -> Self {
        Self {
            max_buf_size,
            protocol,
            client,
        }
    }
}

impl tower::Service<Uri> for DuplexTransport {
    type Response = TransportStream<Stream>;

    type Error = io::Error;

    type Future = BoxFuture<'static, Result<TransportStream<Stream>, io::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: Uri) -> Self::Future {
        let client = self.client.clone();
        let max_buf_size = self.max_buf_size;
        let protocol = self.protocol.clone();
        let fut = async move {
            let stream = client.connect(max_buf_size, protocol).await?;
            TransportStream::new_stream(stream.into()).await
        };

        Box::pin(fut)
    }
}
