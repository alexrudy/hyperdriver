//! A connection type which can notify the user when it is ready.
use std::fmt;
use std::task::Context;
use std::task::Poll;

use thiserror::Error;
use tokio::sync::watch;

use self::future::SendAndWatchFuture;

use super::Connection;
use super::PoolableConnection;

/// A connection which can be monitored for readiness.
#[derive(Clone)]
pub struct WatchedConnection<C> {
    connection: C,
    sender: watch::Sender<bool>,
    receiver: watch::Receiver<bool>,
}

impl<C> fmt::Debug for WatchedConnection<C>
where
    C: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("WatchedConnection")
            .field(&self.connection)
            .finish()
    }
}

impl<C> WatchedConnection<C> {
    /// Create a new notifying connection.
    pub fn new(connection: C) -> Self {
        let (sender, receiver) = watch::channel(false);
        Self {
            connection,
            sender,
            receiver,
        }
    }
}

impl<C> WatchedConnection<C> {
    /// Get a connection watcher.
    pub fn watch(&self) -> Watcher {
        Watcher {
            receiver: self.receiver.clone(),
        }
    }
}

impl<C, B> Connection<B> for WatchedConnection<C>
where
    C: Connection<B>,
{
    type ResBody = C::ResBody;
    type Error = C::Error;
    type Future = SendAndWatchFuture<C::Future, C::ResBody, C::Error>;

    fn send_request(&mut self, request: http::Request<B>) -> Self::Future {
        if let Err(err) = self.sender.send(false) {
            tracing::error!("Error sending notification for watched connection: {}", err)
        };

        let watcher = self.watch();

        SendAndWatchFuture::new(
            Connection::send_request(&mut self.connection, request),
            watcher,
        )
    }

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match Connection::poll_ready(&mut self.connection, cx) {
            Poll::Ready(Ok(())) => {
                if let Err(err) = self.sender.send(true) {
                    tracing::error!("Error sending notification for watched connection: {}", err);
                };
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }

    fn version(&self) -> http::Version {
        Connection::version(&self.connection)
    }
}

impl<C, B> PoolableConnection<B> for WatchedConnection<C>
where
    C: PoolableConnection<B> + Unpin + Send + Sync + 'static,
    B: Send + 'static,
{
    fn is_open(&self) -> bool {
        PoolableConnection::is_open(&self.connection)
    }

    fn can_share(&self) -> bool {
        PoolableConnection::can_share(&self.connection)
    }

    fn reuse(&mut self) -> Option<Self> {
        PoolableConnection::reuse(&mut self.connection).map(|conn| WatchedConnection {
            connection: conn,
            sender: self.sender.clone(),
            receiver: self.receiver.clone(),
        })
    }
}

/// Connection watcher
#[derive(Debug, Clone)]
pub struct Watcher {
    receiver: watch::Receiver<bool>,
}

impl Watcher {
    /// Asynchronously wait for the underlying connection to be ready.
    pub async fn ready(&mut self) -> Result<(), WatchError> {
        match self.receiver.wait_for(|ready| *ready).await {
            Ok(_) => Ok(()),
            Err(_) => Err(WatchError),
        }
    }

    /// Check if the underlying connection is ready now.
    pub fn is_ready(&self) -> bool {
        *self.receiver.borrow()
    }
}

/// Error returned when a watched connection disappears.
#[derive(Debug, Error)]
#[error("Connection dropped before being ready.")]
pub struct WatchError;

mod future {
    use std::fmt;
    use std::future::Future;
    use std::marker::PhantomData;
    use std::pin::Pin;
    use std::task::{ready, Context, Poll};

    use pin_project::pin_project;

    use crate::DebugLiteral;

    use super::Watcher;

    #[pin_project]
    pub struct SendAndWatchFuture<F, B, E> {
        #[pin]
        future: F,
        watch: Option<Watcher>,
        _body: PhantomData<fn() -> (B, E)>,
    }

    impl<F, B, E> SendAndWatchFuture<F, B, E> {
        pub(super) fn new(future: F, watcher: Watcher) -> Self {
            Self {
                future,
                watch: Some(watcher),
                _body: PhantomData,
            }
        }
    }

    impl<F, B, E> fmt::Debug for SendAndWatchFuture<F, B, E> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match &self.watch {
                Some(watch) => f
                    .debug_struct("SendAndWatchFuture")
                    .field("info", watch)
                    .finish(),
                None => f
                    .debug_struct("SendAndWatchFuture")
                    .field("watch", &DebugLiteral("already polled"))
                    .finish(),
            }
        }
    }

    impl<F, B, E> Future for SendAndWatchFuture<F, B, E>
    where
        F: Future<Output = Result<http::Response<B>, E>>,
    {
        type Output = F::Output;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let mut this = self.project();
            match ready!(this.future.as_mut().poll(cx)) {
                Ok(mut response) => {
                    let watch = this
                        .watch
                        .take()
                        .expect("future already polled for watcher");
                    response.extensions_mut().insert(watch);
                    Poll::Ready(Ok(response))
                }
                Err(error) => Poll::Ready(Err(error)),
            }
        }
    }
}
