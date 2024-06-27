//! A transport full of empty implementations, suitable for testing behavior of transport-dependent code.

use std::future::{ready, Future, Ready};

use thiserror::Error;

use crate::client::conn::stream::mock::MockStream;
use crate::client::pool::PoolableTransport;

use super::TransportStream;

type BoxFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// An error that can occur when creating a mock transport.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("connection error")]
pub struct MockConnectionError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportMode {
    SingleUse,
    Reusable,
    ConnectionError,
}

/// A mock transport that can be used to test connection behavior.
#[derive(Debug, Clone)]
pub struct MockTransport {
    mode: TransportMode,
}

impl PoolableTransport for MockTransport {
    fn can_share(&self) -> bool {
        matches!(self.mode, TransportMode::Reusable)
    }
}

impl MockTransport {
    /// Create a new transport with the specified reuse mode.
    pub fn new(reuse: bool) -> Self {
        let mode = if reuse {
            TransportMode::Reusable
        } else {
            TransportMode::SingleUse
        };
        Self { mode }
    }

    /// Transport which can be used only once
    pub fn single() -> Ready<Result<Self, MockConnectionError>> {
        ready(Ok(Self::new(false)))
    }

    /// Transport which can be reused
    pub fn reusable() -> Ready<Result<Self, MockConnectionError>> {
        ready(Ok(Self::new(true)))
    }

    /// Return a future representing the completion of the connection handshake.
    ///
    /// This must return a boxed future for compatibility with the underlying
    /// pool.
    pub fn handshake(self) -> BoxFuture<'static, Result<MockStream, MockConnectionError>> {
        let reuse = matches!(self.mode, TransportMode::Reusable);
        Box::pin(ready(Ok(MockStream::new(reuse))))
    }

    /// Transport which returns an error during connection attempts
    pub fn error() -> Ready<Result<Self, MockConnectionError>> {
        ready(Err(MockConnectionError))
    }

    /// Transport which immediately returns an error.
    pub fn connection_error() -> Self {
        Self {
            mode: TransportMode::ConnectionError,
        }
    }
}

impl tower::Service<http::Uri> for MockTransport {
    type Response = TransportStream<MockStream>;

    type Error = MockConnectionError;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: http::Uri) -> Self::Future {
        let reuse = match self.mode {
            TransportMode::SingleUse => false,
            TransportMode::Reusable => true,
            TransportMode::ConnectionError => return Box::pin(ready(Err(MockConnectionError))),
        };

        let conn = MockStream::new(reuse);
        Box::pin(async move {
            Ok(TransportStream::new(
                conn,
                #[cfg(feature = "tls")]
                None,
            ))
        })
    }
}

#[cfg(test)]
mod tests {

    use crate::client::conn::Transport;

    use super::*;

    use static_assertions::assert_impl_all;

    assert_impl_all!(MockConnectionError: std::error::Error, Send, Sync);
    assert_impl_all!(MockTransport: Transport);
}
