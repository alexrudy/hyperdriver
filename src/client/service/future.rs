use std::fmt;
use std::future::Future;
use std::task::Poll;

use futures_core::future::BoxFuture;
use futures_util::FutureExt as _;

use crate::client::conn::{connection::ConnectionError, Connection};
use crate::client::pool::{self, Checkout};
use crate::client::Error;

/// A future that resolves to an HTTP response.
pub struct ResponseFuture<C, T, BOut = crate::Body>
where
    C: pool::PoolableConnection,
    T: pool::PoolableTransport,
{
    inner: ResponseFutureState<C, T>,
    _body: std::marker::PhantomData<fn() -> BOut>,
}

impl<C: pool::PoolableConnection, T: pool::PoolableTransport, B> fmt::Debug
    for ResponseFuture<C, T, B>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponseFuture").finish()
    }
}

impl<C, T, BOut> ResponseFuture<C, T, BOut>
where
    C: pool::PoolableConnection,
    T: pool::PoolableTransport,
{
    pub(super) fn new(
        checkout: Checkout<C, T, ConnectionError>,
        request: crate::body::Request,
    ) -> Self {
        Self {
            inner: ResponseFutureState::Checkout { checkout, request },
            _body: std::marker::PhantomData,
        }
    }

    pub(super) fn error(error: ConnectionError) -> Self {
        Self {
            inner: ResponseFutureState::ConnectionError(error),
            _body: std::marker::PhantomData,
        }
    }
}

impl<C, T, BOut> Future for ResponseFuture<C, T, BOut>
where
    C: Connection + pool::PoolableConnection,
    C::ResBody: Into<crate::body::Body>,
    T: pool::PoolableTransport,
    BOut: From<crate::body::Body> + Unpin,
{
    type Output = Result<http::Response<BOut>, Error>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        loop {
            match std::mem::replace(&mut self.inner, ResponseFutureState::Empty) {
                ResponseFutureState::Checkout {
                    mut checkout,
                    request,
                } => match checkout.poll_unpin(cx) {
                    Poll::Ready(Ok(conn)) => {
                        self.inner = ResponseFutureState::Request(
                            super::execute_request(request, conn).boxed(),
                        );
                    }
                    Poll::Ready(Err(error)) => {
                        return Poll::Ready(Err(error.into()));
                    }
                    Poll::Pending => {
                        self.inner = ResponseFutureState::Checkout { checkout, request };
                        return Poll::Pending;
                    }
                },
                ResponseFutureState::Request(mut fut) => match fut.poll_unpin(cx) {
                    Poll::Ready(Ok(response)) => return Poll::Ready(Ok(response.map(Into::into))),
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Pending => {
                        self.inner = ResponseFutureState::Request(fut);
                        return Poll::Pending;
                    }
                },
                ResponseFutureState::ConnectionError(error) => {
                    return Poll::Ready(Err(Error::Connection(error.into())));
                }
                ResponseFutureState::Empty => {
                    panic!("future polled after completion");
                }
            }
        }
    }
}

enum ResponseFutureState<C: pool::PoolableConnection, T: pool::PoolableTransport> {
    Empty,
    Checkout {
        checkout: Checkout<C, T, ConnectionError>,
        request: crate::body::Request,
    },
    ConnectionError(ConnectionError),
    Request(BoxFuture<'static, Result<http::Response<crate::body::Body>, Error>>),
}
