//! Mock protocol implementation for testing purposes.

use std::future::{ready, Ready};

use thiserror::Error;

use crate::conn::connection::ConnectionError;
use crate::conn::stream::mock::MockStream;
use crate::conn::Connection;
use crate::pool::PoolableConnection;

use super::ProtocolRequest;

/// A minimal protocol sender for testing purposes.
#[derive(Debug, Clone)]
pub struct MockSender;

impl Connection for MockSender {
    type ResBody = crate::Body;

    type Error = MockProtocolError;

    type Future = Ready<Result<hyperdriver_body::Response, Self::Error>>;

    fn send_request(&mut self, request: hyperdriver_body::Request) -> Self::Future {
        ready(Ok(hyperdriver_body::Response::new(request.into_body())))
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
#[derive(Debug, Error, PartialEq, Eq)]
#[error("mock protocol error")]
pub struct MockProtocolError;

/// A simple protocol for returning empty responses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MockProtocol;

impl tower::Service<ProtocolRequest<MockStream>> for MockProtocol {
    type Response = MockSender;

    type Error = ConnectionError;
    type Future = Ready<Result<MockSender, ConnectionError>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: ProtocolRequest<MockStream>) -> Self::Future {
        ready(Ok(MockSender))
    }
}

#[cfg(test)]
mod tests {
    use crate::conn::Protocol;

    use super::*;

    use static_assertions::assert_impl_all;

    assert_impl_all!(MockSender: Connection, PoolableConnection);
    assert_impl_all!(MockProtocol: Protocol<MockStream>);
}
