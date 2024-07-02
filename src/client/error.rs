use thiserror::Error;

use super::conn::connection::ConnectionError;
use super::{pool, BoxError};

/// Client error type.
#[derive(Debug, Error)]
pub enum Error {
    /// Error occured with the underlying connection.
    #[error("connection: {0}")]
    Connection(#[source] BoxError),

    /// Error occured with the underlying transport.
    #[error("transport: {0}")]
    Transport(#[source] BoxError),

    /// Error occured with the underlying protocol.
    #[error("protocol: {0}")]
    Protocol(#[source] BoxError),

    /// Error occured with the underlying service
    #[error("serivce: {0}")]
    Service(#[source] BoxError),

    /// Error occured with the user's request, such as an invalid URI.
    #[error("user error: {0}")]
    User(#[source] hyper::Error),

    /// Invalid HTTP Method for the current action.
    #[error("invalid method: {0}")]
    InvalidMethod(http::Method),

    /// Protocol is not supported by this client or transport.
    #[error("unsupported protocol")]
    UnsupportedProtocol,
}

impl From<pool::Error<ConnectionError>> for Error {
    fn from(error: pool::Error<ConnectionError>) -> Self {
        match error {
            pool::Error::Connecting(error) => Error::Connection(error.into()),
            pool::Error::Handshaking(error) => Error::Transport(error.into()),
            pool::Error::Unavailable => {
                Error::Connection("pool closed, no connection can be made".into())
            }
        }
    }
}

fn try_downcast<T, K>(k: K) -> Result<T, K>
where
    T: 'static,
    K: 'static,
{
    let mut k = Some(k);
    if let Some(k) = <dyn std::any::Any>::downcast_mut::<Option<T>>(&mut k) {
        Ok(k.take().unwrap())
    } else {
        Err(k.unwrap())
    }
}

impl Error {
    pub(crate) fn downcast(error: BoxError) -> Self {
        let error = match try_downcast::<Self, BoxError>(error) {
            Ok(error) => return error,
            Err(error) => error,
        };

        let error = match try_downcast::<ConnectionError, BoxError>(error) {
            Ok(error) => return Error::Connection(error.into()),
            Err(error) => error,
        };

        let error = match try_downcast::<pool::Error<ConnectionError>, BoxError>(error) {
            Ok(error) => return error.into(),
            Err(error) => error,
        };

        let error = match try_downcast::<hyper::Error, BoxError>(error) {
            Ok(error) => return Error::User(error),
            Err(error) => error,
        };

        Error::Service(error)
    }
}

#[derive(Debug, Clone)]
pub struct DowncastError<S> {
    inner: S,
}

impl<S> DowncastError<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S, R> tower::Service<R> for DowncastError<S>
where
    S: tower::Service<R, Error = BoxError>,
{
    type Response = S::Response;
    type Error = Error;
    type Future = future::DowncastErrorFuture<S::Future>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner
            .poll_ready(cx)
            .map(|r| r.map_err(Error::downcast))
    }

    fn call(&mut self, req: R) -> Self::Future {
        future::DowncastErrorFuture {
            inner: self.inner.call(req),
        }
    }
}

mod future {
    use std::future::Future;

    use super::BoxError;

    #[pin_project::pin_project]
    pub struct DowncastErrorFuture<F> {
        #[pin]
        pub(super) inner: F,
    }

    impl<F, T> Future for DowncastErrorFuture<F>
    where
        F: Future<Output = Result<T, BoxError>>,
    {
        type Output = Result<T, super::Error>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            self.project()
                .inner
                .poll(cx)
                .map(|r| r.map_err(super::Error::downcast))
        }
    }
}

#[derive(Debug, Clone)]
pub struct DowncastErrorLayer {
    _priv: (),
}

impl DowncastErrorLayer {
    pub fn new() -> Self {
        Self { _priv: () }
    }
}

impl<S> tower::layer::Layer<S> for DowncastErrorLayer {
    type Service = DowncastError<S>;

    fn layer(&self, inner: S) -> Self::Service {
        DowncastError::new(inner)
    }
}
