//! Middleware which applies a timeout to requests.
//!
//! Supports a custom error type.

use std::time::Duration;

/// Layer to apply a timeout to requests, with a custom error type.
pub struct TimeoutLayer<E> {
    error: Box<fn() -> E>,
    timeout: Duration,
}

impl<E> TimeoutLayer<E> {
    /// Create a new `TimeoutLayer` with the provided error function and timeout.
    pub fn new(error: fn() -> E, timeout: Duration) -> Self {
        Self {
            error: Box::new(error),
            timeout,
        }
    }
}

impl<E> Clone for TimeoutLayer<E> {
    fn clone(&self) -> Self {
        Self {
            error: self.error.clone(),
            timeout: self.timeout,
        }
    }
}

impl<E> std::fmt::Debug for TimeoutLayer<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimeoutLayer")
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl<S, E> tower::layer::Layer<S> for TimeoutLayer<E> {
    type Service = Timeout<S, E>;

    fn layer(&self, inner: S) -> Self::Service {
        Timeout::new(inner, self.timeout, self.error.clone())
    }
}

/// Applies a timeout to requests, with a custom error type.
pub struct Timeout<S, E> {
    inner: S,
    timeout: Duration,
    error: Box<fn() -> E>,
}

impl<S, E> Timeout<S, E> {
    /// Create a new `Timeout` with the provided inner service, timeout, and error function.
    pub fn new(inner: S, timeout: Duration, error: Box<fn() -> E>) -> Self {
        Self {
            inner,
            timeout,
            error,
        }
    }
}

impl<S, E> Clone for Timeout<S, E>
where
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            timeout: self.timeout,
            error: self.error.clone(),
        }
    }
}

impl<S, E> std::fmt::Debug for Timeout<S, E>
where
    S: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Timeout")
            .field("inner", &self.inner)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl<S, E, Req> tower::Service<Req> for Timeout<S, E>
where
    S: tower::Service<Req, Error = E>,
{
    type Response = S::Response;
    type Error = E;
    type Future = self::future::TimeoutFuture<S::Future, S::Response, E>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        self::future::TimeoutFuture::new(self.inner.call(req), self.error.clone(), self.timeout)
    }
}

mod future {

    use std::{future::Future, marker::PhantomData, task::Poll};

    use pin_project::pin_project;

    #[derive(Debug)]
    #[pin_project]
    pub struct TimeoutFuture<F, R, E> {
        #[pin]
        inner: F,
        error: Box<fn() -> E>,
        response: PhantomData<fn() -> R>,

        #[pin]
        timeout: tokio::time::Sleep,
    }

    impl<F, R, E> TimeoutFuture<F, R, E> {
        pub fn new(inner: F, error: Box<fn() -> E>, timeout: std::time::Duration) -> Self {
            Self {
                inner,
                error,
                response: PhantomData,
                timeout: tokio::time::sleep(timeout),
            }
        }
    }

    impl<F, R, E> Future for TimeoutFuture<F, R, E>
    where
        F: Future<Output = Result<R, E>>,
    {
        type Output = Result<R, E>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> Poll<Self::Output> {
            let this = self.project();

            match this.inner.poll(cx) {
                Poll::Ready(response) => return Poll::Ready(response),
                Poll::Pending => {}
            }

            match this.timeout.poll(cx) {
                Poll::Ready(()) => Poll::Ready(Err((this.error)())),
                Poll::Pending => Poll::Pending,
            }
        }
    }
}
