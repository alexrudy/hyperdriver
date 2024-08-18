//! A transport full of empty implementations, suitable for testing behavior of transport-dependent code.

use std::future::{ready, Future};

use thiserror::Error;

use crate::client::conn::stream::mock::{MockConnection, MockStream};
use crate::client::pool::{self};

type BoxFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// An error that can occur when creating a mock transport.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("connection error")]
pub struct MockConnectionError;

#[derive(Debug)]
enum TransportMode {
    SingleUse,
    Reusable,
    ConnectionError,
    Channel(tokio::sync::oneshot::Receiver<MockStream>),
}

/// A mock transport that can be used to test connection behavior.
#[derive(Debug)]
pub struct MockTransport {
    mode: TransportMode,
}

impl Clone for MockTransport {
    fn clone(&self) -> Self {
        Self {
            mode: match &self.mode {
                TransportMode::Channel(_) => TransportMode::ConnectionError,
                TransportMode::SingleUse => TransportMode::SingleUse,
                TransportMode::Reusable => TransportMode::Reusable,
                TransportMode::ConnectionError => TransportMode::ConnectionError,
            },
        }
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
    pub fn single() -> Self {
        Self::new(false)
    }

    /// Transport which can be reused
    pub fn reusable() -> Self {
        Self::new(true)
    }

    /// Transport which returns an error during connection attempts
    pub fn error() -> Self {
        Self {
            mode: TransportMode::ConnectionError,
        }
    }

    /// Transport which returns a stream from a oneshot channel
    pub fn channel(rx: tokio::sync::oneshot::Receiver<MockStream>) -> Self {
        Self {
            mode: TransportMode::Channel(rx),
        }
    }

    /// Create a new connector for the transport.
    pub fn connector(self) -> pool::Connector<MockConnection, MockStream, MockConnectionError> {
        let connect: BoxFuture<'static, Result<MockStream, MockConnectionError>> = match self.mode {
            TransportMode::SingleUse => Box::pin(ready(Ok(MockStream::single()))) as _,
            TransportMode::Reusable => Box::pin(ready(Ok(MockStream::reusable()))) as _,
            TransportMode::ConnectionError => Box::pin(ready(Err(MockConnectionError))) as _,
            TransportMode::Channel(rx) => Box::pin(async move {
                Ok(rx
                    .await
                    .expect("mock channel closed before stream was received"))
            }) as _,
        };

        let handshake = move |stream| Box::pin(ready(Ok(MockConnection::new(stream)))) as _;

        pool::Connector::new(move || connect, handshake)
    }
}

impl tower::Service<http::Uri> for MockTransport {
    type Response = MockStream;

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
            TransportMode::Channel(_) => return Box::pin(ready(Err(MockConnectionError))),
        };

        let conn = MockStream::new(reuse);
        Box::pin(async move { Ok(conn) })
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
