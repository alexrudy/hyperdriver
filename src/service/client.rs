//! Core service for implementing a Client.
//!
//! This service accepts an `ExecuteRequest` and returns a `http::Response`.
//!
//! The `ExecuteRequest` contains the connection to use for the request and the
//! request to execute.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

use crate::BoxFuture;
use http_body::Body;
use hyper::rt::Executor;
use tracing::Instrument;

use crate::client::conn::Connection;
use crate::client::pool::PoolableConnection;
use crate::client::pool::Pooled;
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
pub struct ExecuteRequest<C: Connection<B> + PoolableConnection, B> {
    /// The connection to use for the request.
    conn: Pooled<C>,
    /// The request to execute.
    request: http::Request<B>,
}

impl<C: Connection<B> + PoolableConnection, B> ExecuteRequest<C, B> {
    /// Create a new request
    pub fn new(conn: Pooled<C>, request: http::Request<B>) -> Self {
        Self { conn, request }
    }

    /// Split the request into its parts.
    pub fn into_parts(self) -> (Pooled<C>, http::Request<B>) {
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
pub struct RequestExecutor<C: Connection<B> + PoolableConnection, B, E> {
    executor: E,
    _private: std::marker::PhantomData<fn(C, B) -> ()>,
}

impl<C: Connection<B> + PoolableConnection, B, E> fmt::Debug for RequestExecutor<C, B, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestExecutor").finish()
    }
}

impl<C: Connection<B> + PoolableConnection, B, E: Clone> Clone for RequestExecutor<C, B, E> {
    fn clone(&self) -> Self {
        Self {
            executor: self.executor.clone(),
            _private: std::marker::PhantomData,
        }
    }
}

impl<C: Connection<B> + PoolableConnection, B, E> RequestExecutor<C, B, E> {
    /// Create a new `RequestExecutor`.
    pub fn new(executor: E) -> Self {
        Self {
            executor,
            _private: std::marker::PhantomData,
        }
    }
}

impl<C, B, E> tower::Service<ExecuteRequest<C, B>> for RequestExecutor<C, B, E>
where
    C: Connection<B> + PoolableConnection + Send + 'static,
    B: Body + Unpin + Send + 'static,
    E: Executor<WhenReady<C, B>> + Clone + Send + 'static,
{
    type Response = http::Response<C::ResBody>;

    type Error = Error;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: ExecuteRequest<C, B>) -> Self::Future {
        Box::pin(execute_request(req, self.executor.clone()))
    }
}

async fn execute_request<C, BIn, BOut, E>(
    ExecuteRequest { request, mut conn }: ExecuteRequest<C, BIn>,
    executor: E,
) -> Result<http::Response<BOut>, Error>
where
    C: Connection<BIn, ResBody = BOut> + PoolableConnection,
    BIn: 'static,
    E: Executor<WhenReady<C, BIn>>,
{
    let span = tracing::trace_span!("send request", request.uri=%request.uri());
    tracing::trace!(parent: &span, request.uri=%request.uri(), conn.version=?conn.version(), req.version=?request.version(), "sending request");

    let response = conn
        .send_request(request)
        .instrument(span)
        .await
        .map_err(|error| Error::Connection(error.into()))?;

    // Shared connections are already in the pool, no need to do this.
    if !conn.can_share() {
        // Only re-insert the connection when it is ready again. Spawn
        // a task to wait for the connection to become ready before dropping.
        executor.execute(WhenReady::new(conn));
    }

    Ok(response.map(Into::into))
}

/// A future which resolves when the connection is ready again
#[derive(Debug)]
pub struct WhenReady<C: Connection<B> + PoolableConnection, B> {
    conn: Pooled<C>,
    _private: std::marker::PhantomData<fn(B)>,
}

impl<C: Connection<B> + PoolableConnection, B> WhenReady<C, B> {
    pub(crate) fn new(conn: Pooled<C>) -> Self {
        Self {
            conn,
            _private: std::marker::PhantomData,
        }
    }
}

impl<C: Connection<B> + PoolableConnection, B> Future for WhenReady<C, B> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        match self.conn.poll_ready(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(()),
            Poll::Ready(Err(err)) => {
                tracing::trace!(error = %err, "Connection errored while polling for readiness");
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
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
        use crate::client::{conn::transport::mock::MockExecutor, ConnectionPoolService};

        let transport = MockTransport::new(false);
        let protocol = MockProtocol::default();
        let pool = PoolConfig::default();

        let client: ConnectionPoolService<MockTransport, MockProtocol, _, Body, MockExecutor> =
            ConnectionPoolService::new(
                transport,
                protocol,
                RequestExecutor::new(MockExecutor),
                pool,
                MockExecutor,
            );

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
        use crate::client::{conn::transport::mock::MockExecutor, ConnectionPoolService};

        let transport = MockTransport::error();
        let protocol = MockProtocol::default();
        let pool = PoolConfig::default();

        let client: ConnectionPoolService<MockTransport, MockProtocol, _, Body, MockExecutor> =
            ConnectionPoolService::new(
                transport,
                protocol,
                RequestExecutor::new(MockExecutor),
                pool,
                MockExecutor,
            );

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
            panic!("unexpected error: {:?}", err);
        };

        err.downcast::<MockConnectionError>().unwrap();
    }
}
