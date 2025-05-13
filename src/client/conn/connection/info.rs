//! Expose connection info the response http Extensions.

use self::future::SendWithInfoFuture;
use super::{Connection, PoolableConnection};
use crate::info::ConnectionInfo;

/// A connection with associated info
///
/// Wrap a connection and its associated info,
/// inserting the info into the response extensions
/// so that consumers of the client can inspect it.
#[derive(Debug, Clone)]
pub struct WithInfo<C, A> {
    connection: C,
    info: ConnectionInfo<A>,
}

impl<C, A> WithInfo<C, A> {
    /// Wrap a connection with asssociated transport info.
    pub fn new(connection: C, info: ConnectionInfo<A>) -> Self {
        Self { connection, info }
    }

    /// Unwrap the connection and info objects.
    pub fn into_inner(self) -> (C, ConnectionInfo<A>) {
        (self.connection, self.info)
    }

    /// Access the inner connection
    pub fn connection(&self) -> &C {
        &self.connection
    }

    /// Access a mutable reference to the inner connection
    pub fn connection_mut(&mut self) -> &mut C {
        &mut self.connection
    }

    /// Access the connection info
    pub fn info(&self) -> &ConnectionInfo<A> {
        &self.info
    }
}

impl<B, C, A> Connection<B> for WithInfo<C, A>
where
    C: Connection<B>,
    A: Clone + Send + Sync + 'static,
{
    type ResBody = C::ResBody;
    type Error = C::Error;
    type Future = SendWithInfoFuture<C::Future, C::ResBody, C::Error, A>;

    fn send_request(&mut self, request: http::Request<B>) -> Self::Future {
        SendWithInfoFuture::new(self.connection.send_request(request), self.info.clone())
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Connection::poll_ready(&mut self.connection, cx)
    }

    fn version(&self) -> http::Version {
        Connection::version(&self.connection)
    }
}

impl<B, C, A> PoolableConnection<B> for WithInfo<C, A>
where
    C: PoolableConnection<B>,
    B: Send + 'static,
    A: Clone + Send + Sync + Unpin + 'static,
{
    fn is_open(&self) -> bool {
        PoolableConnection::is_open(&self.connection)
    }

    fn can_share(&self) -> bool {
        PoolableConnection::can_share(&self.connection)
    }

    fn reuse(&mut self) -> Option<Self> {
        PoolableConnection::reuse(&mut self.connection).map(|c| WithInfo {
            connection: c,
            info: self.info.clone(),
        })
    }
}

/// Opaque future for connections
mod future {
    use std::fmt;
    use std::future::Future;
    use std::marker::PhantomData;
    use std::pin::Pin;
    use std::task::{ready, Context, Poll};

    use pin_project::pin_project;

    use crate::info::ConnectionInfo;
    use crate::DebugLiteral;

    #[pin_project]
    pub struct SendWithInfoFuture<F, B, E, A> {
        #[pin]
        future: F,
        info: Option<ConnectionInfo<A>>,
        _body: PhantomData<fn() -> (B, E)>,
    }

    impl<F, B, E, A> SendWithInfoFuture<F, B, E, A> {
        pub(super) fn new(future: F, info: ConnectionInfo<A>) -> Self {
            Self {
                future,
                info: Some(info),
                _body: PhantomData,
            }
        }
    }

    impl<F, B, E, A> fmt::Debug for SendWithInfoFuture<F, B, E, A>
    where
        A: fmt::Debug,
    {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match &self.info {
                Some(info) => f
                    .debug_struct("SendWithInfoFuture")
                    .field("info", info)
                    .finish(),
                None => f
                    .debug_struct("SendWithInfoFuture")
                    .field("info", &DebugLiteral("already polled"))
                    .finish(),
            }
        }
    }

    impl<F, B, E, A> Future for SendWithInfoFuture<F, B, E, A>
    where
        F: Future<Output = Result<http::Response<B>, E>>,
        A: Clone + Send + Sync + 'static,
    {
        type Output = F::Output;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let mut this = self.project();
            match ready!(this.future.as_mut().poll(cx)) {
                Ok(mut response) => {
                    let info = this.info.take().expect("future already polled for info");
                    response.extensions_mut().insert(info);
                    Poll::Ready(Ok(response))
                }
                Err(error) => Poll::Ready(Err(error)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Body;
    use crate::BoxFuture;

    use super::Connection;

    static_assertions::assert_obj_safe!(Connection<Body, Future=BoxFuture<'static, ()>, Error=std::io::Error, ResBody=Body>);
}
