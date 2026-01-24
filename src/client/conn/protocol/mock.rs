//! Mock protocol implementation for testing purposes.

use std::convert::Infallible;
use std::future::{Ready, ready};

use thiserror::Error;

use crate::client::conn::Connection;
use crate::client::conn::stream::mock::{MockStream, StreamID};
use chateau::client::pool::{PoolableConnection, PoolableStream};

/// A minimal protocol sender for testing purposes.
#[derive(Debug, Clone)]
pub struct MockSender {
    id: StreamID,
    stream: MockStream,
}

impl MockSender {
    /// The unique identifier for the stream.
    pub fn id(&self) -> StreamID {
        self.id
    }

    /// Close the connection and stream
    pub fn close(&self) {
        self.stream.close();
    }

    /// Create a single-use mock sender.
    pub fn single() -> Self {
        Self {
            id: StreamID::new(),
            stream: MockStream::single(),
        }
    }

    /// Create a new reusable mock sender.
    pub fn reusable() -> Self {
        Self {
            id: StreamID::new(),
            stream: MockStream::reusable(),
        }
    }

    /// Create a new mock sender.
    pub fn new() -> Self {
        Self::reusable()
    }
}

impl Default for MockSender {
    fn default() -> Self {
        Self::reusable()
    }
}

impl Connection<http::Request<crate::Body>> for MockSender {
    type Response = http::Response<crate::Body>;

    type Error = MockProtocolError;

    type Future = Ready<Result<http::Response<crate::Body>, Self::Error>>;

    fn send_request(&mut self, request: http::Request<crate::Body>) -> Self::Future {
        ready(Ok(http::Response::new(request.into_body())))
    }

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
}

impl PoolableConnection<http::Request<crate::Body>> for MockSender {
    fn is_open(&self) -> bool {
        self.stream.is_open()
    }

    fn can_share(&self) -> bool {
        self.stream.can_share()
    }

    fn reuse(&mut self) -> Option<Self> {
        Some(self.clone())
    }
}

/// Error type for the mock protocol.
#[derive(Debug, Default, Error, PartialEq, Eq)]
#[error("mock protocol error")]
pub struct MockProtocolError {
    _private: (),
}

/// A simple protocol for returning empty responses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MockProtocol {
    _private: (),
}

impl tower::Service<MockStream> for MockProtocol {
    type Response = MockSender;

    type Error = Infallible;
    type Future = Ready<Result<MockSender, Infallible>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, stream: MockStream) -> Self::Future {
        ready(Ok(MockSender {
            id: StreamID::new(),
            stream,
        }))
    }
}

#[cfg(test)]
mod tests {
    use crate::client::conn::Protocol;

    use super::*;

    use static_assertions::assert_impl_all;

    assert_impl_all!(MockSender: Connection<http::Request<crate::Body>>, PoolableConnection<http::Request<crate::Body>>);
    assert_impl_all!(MockProtocol: Protocol<MockStream, http::Request<crate::Body>>);
}
