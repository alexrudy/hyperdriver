//! Core service for implementing a Client.
//!
//! This service accepts an `ExecuteRequest` and returns a `http::Response`.
//!
//! The `ExecuteRequest` contains the connection to use for the request and the
//! request to execute.

use std::fmt;
use std::task::Poll;

use crate::BoxFuture;
use http_body::Body;
use tracing::Instrument;

use crate::client::conn::Connection;
use crate::client::Error;

/// Couples the connection with the http request.
///
/// This allows downstream services to access both the request and connection
/// -- they are needed in tandem to send the request. This also means that
/// middleware can implement [`tower::Service`] on `tower::Service<ExecuteRequest<C, B>>`
/// to modify the request before it is sent in the context of the connection.
///
/// See [`crate::service::SetHostHeader`] for an example of a middleware that modifies
/// the request before it is sent in the context of the connection.
#[derive(Debug)]
pub struct ExecuteRequest<C: Connection<B>, B> {
    /// The connection to use for the request.
    conn: C,
    /// The request to execute.
    request: http::Request<B>,
}

impl<C: Connection<B>, B> ExecuteRequest<C, B> {
    /// Create a new request
    pub fn new(conn: C, request: http::Request<B>) -> Self {
        Self { conn, request }
    }

    /// Split the request into its parts.
    pub fn into_parts(self) -> (C, http::Request<B>) {
        (self.conn, self.request)
    }

    /// A reference to the connection.
    pub fn connection(&self) -> &C {
        &self.conn
    }

    /// A reference to the request.
    pub fn request(&self) -> &http::Request<B> {
        &self.request
    }

    /// A mutable reference to the request.
    pub fn request_mut(&mut self) -> &mut http::Request<B> {
        &mut self.request
    }
}

/// A service to execute a request on a hyper connection.
///
/// A service which executes a request on a `hyper` Connection as described
/// by the `Connection` trait. This should be the innermost service
/// for clients, as it is responsible for actually sending the request.
pub struct RequestExecutor<C: Connection<B>, B> {
    _private: std::marker::PhantomData<fn(C, B) -> ()>,
}

impl<C: Connection<B>, B> Default for RequestExecutor<C, B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: Connection<B>, B> fmt::Debug for RequestExecutor<C, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestExecutor").finish()
    }
}

impl<C: Connection<B>, B> Clone for RequestExecutor<C, B> {
    fn clone(&self) -> Self {
        Self::new()
    }
}

impl<C: Connection<B>, B> RequestExecutor<C, B> {
    /// Create a new `RequestExecutor`.
    pub fn new() -> Self {
        Self {
            _private: std::marker::PhantomData,
        }
    }
}

impl<C, B> tower::Service<ExecuteRequest<C, B>> for RequestExecutor<C, B>
where
    C: Connection<B> + Send + 'static,
    B: Body + Unpin + Send + 'static,
{
    type Response = http::Response<C::ResBody>;

    type Error = Error;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: ExecuteRequest<C, B>) -> Self::Future {
        Box::pin(execute_request(req))
    }
}

async fn execute_request<C, BIn, BOut>(
    ExecuteRequest { request, mut conn }: ExecuteRequest<C, BIn>,
) -> Result<http::Response<BOut>, Error>
where
    C: Connection<BIn, ResBody = BOut> + Send,
    BIn: 'static,
{
    let span = tracing::trace_span!("send request", request.uri=%request.uri());
    tracing::trace!(parent: &span, request.uri=%request.uri(), conn.version=?conn.version(), req.version=?request.version(), "sending request");

    let response = conn
        .send_request(request)
        .instrument(span)
        .await
        .map_err(|error| Error::Connection(error.into()))?;

    Ok(response.map(Into::into))
}

#[cfg(all(test, feature = "mocks"))]
mod tests {

    use crate::Body;

    use crate::client::conn::protocol::mock::MockProtocol;
    use crate::client::conn::transport::mock::{MockConnectionError, MockTransport};
    use crate::client::pool::Config as PoolConfig;

    use super::*;

    #[tokio::test]
    async fn test_client_mock_transport() {
        use crate::client::ConnectionPoolService;

        let transport = MockTransport::new(false);
        let protocol = MockProtocol::default();
        let pool = PoolConfig::default();

        let client: ConnectionPoolService<MockTransport, MockProtocol, _, Body> =
            ConnectionPoolService::new(transport, protocol, RequestExecutor::new(), pool);

        client
            .request(
                http::Request::builder()
                    .uri("mock://somewhere")
                    .body(crate::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_client_mock_connection_error() {
        use crate::client::ConnectionPoolService;

        let transport = MockTransport::error();
        let protocol = MockProtocol::default();
        let pool = PoolConfig::default();

        let client: ConnectionPoolService<MockTransport, MockProtocol, _, Body> =
            ConnectionPoolService::new(transport, protocol, RequestExecutor::new(), pool);

        let result = client
            .request(
                http::Request::builder()
                    .uri("mock://somewhere")
                    .body(crate::Body::empty())
                    .unwrap(),
            )
            .await;

        let err = result.unwrap_err();

        let Error::Connection(err) = err else {
            panic!("unexpected error: {err:?}");
        };

        err.downcast::<MockConnectionError>().unwrap();
    }
}
