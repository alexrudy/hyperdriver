//! Mock protocol implementation for testing purposes.

use std::future::{ready, Ready};

use thiserror::Error;

use crate::client::conn::connection::ConnectionError;
use crate::client::conn::stream::mock::MockStream;
use crate::client::conn::Connection;
use crate::client::pool::PoolableConnection;

use super::ProtocolRequest;

/// A minimal protocol sender for testing purposes.
#[derive(Debug, Default, Clone)]
pub struct MockSender {
    _private: (),
}

impl Connection<crate::Body> for MockSender {
    type ResBody = crate::Body;

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

    fn version(&self) -> http::Version {
        http::Version::HTTP_11
    }
}

impl PoolableConnection for MockSender {
    fn is_open(&self) -> bool {
        true
    }

    fn can_share(&self) -> bool {
        true
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

impl tower::Service<ProtocolRequest<MockStream, crate::Body>> for MockProtocol {
    type Response = MockSender;

    type Error = ConnectionError;
    type Future = Ready<Result<MockSender, ConnectionError>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: ProtocolRequest<MockStream, crate::Body>) -> Self::Future {
        ready(Ok(MockSender::default()))
    }
}

#[cfg(test)]
mod tests {
    use crate::client::conn::Protocol;

    use super::*;

    use static_assertions::assert_impl_all;

    assert_impl_all!(MockSender: Connection<crate::Body>, PoolableConnection);
    assert_impl_all!(MockProtocol: Protocol<MockStream, crate::Body>);
}
