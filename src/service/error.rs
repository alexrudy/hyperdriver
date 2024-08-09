//! Helpers for middleware that might error.

use std::fmt;

pub use self::future::MaybeErrorFuture;

/// A middleware that calls some (non-async) function before
/// calling the inner service, and skips the inner service if the function
/// returns an error.
#[derive(Clone)]
pub struct PreprocessService<S, F> {
    inner: S,
    preprocessor: F,
}

impl<S: fmt::Debug, F> fmt::Debug for PreprocessService<S, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreprocessService")
            .field("inner", &self.inner)
            .finish()
    }
}

impl<S, F> PreprocessService<S, F> {
    /// Helper Service for middleware that might error.
    pub fn new(inner: S, preprocessor: F) -> Self {
        Self {
            inner,
            preprocessor,
        }
    }

    /// Get a reference to the inner service.
    pub fn service(&self) -> &S {
        &self.inner
    }
}

impl<S, F, R> tower::Service<R> for PreprocessService<S, F>
where
    S: tower::Service<R>,
    F: Fn(R) -> Result<R, S::Error>,
{
    type Response = S::Response;

    type Error = S::Error;

    type Future = MaybeErrorFuture<S::Future, S::Response, S::Error>;

    #[inline]
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    #[inline]
    fn call(&mut self, req: R) -> Self::Future {
        match (self.preprocessor)(req) {
            Ok(req) => MaybeErrorFuture::future(self.inner.call(req)),
            Err(error) => MaybeErrorFuture::error(error),
        }
    }
}

/// A layer that wraps a service with a preprocessor function.
#[derive(Clone)]
pub struct PreprocessLayer<F> {
    preprocessor: F,
}

impl<F> PreprocessLayer<F> {
    /// Create a new `PreprocessLayer` wrapping the given preprocessor function.
    pub fn new(preprocessor: F) -> Self {
        Self { preprocessor }
    }
}

impl<F> fmt::Debug for PreprocessLayer<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreprocessLayer").finish()
    }
}

impl<S, F: Clone> tower::layer::Layer<S> for PreprocessLayer<F> {
    type Service = PreprocessService<S, F>;

    fn layer(&self, inner: S) -> Self::Service {
        PreprocessService::new(inner, self.preprocessor.clone())
    }
}

mod future {

    use std::{fmt, future::Future, marker::PhantomData, task::Poll};

    use pin_project::pin_project;

    #[pin_project(project = MaybeErrorFutureStateProj)]
    enum MaybeErrorFutureState<F, E> {
        Inner(#[pin] F),
        Error(Option<E>),
    }

    impl<F, E: fmt::Debug> fmt::Debug for MaybeErrorFutureState<F, E> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Inner(_) => f.debug_tuple("Inner").finish(),
                Self::Error(error) => f.debug_tuple("Error").field(error).finish(),
            }
        }
    }

    /// Future for when a service either errors before yielding,
    /// or continues. This is us
    #[derive(Debug)]
    #[pin_project]
    pub struct MaybeErrorFuture<F, R, E> {
        #[pin]
        state: MaybeErrorFutureState<F, E>,
        response: PhantomData<fn() -> R>,
    }

    impl<F, R, E> MaybeErrorFuture<F, R, E> {
        /// Create a future that resolves to the contained service
        pub fn future(inner: F) -> Self {
            Self {
                state: MaybeErrorFutureState::Inner(inner),
                response: PhantomData,
            }
        }

        /// Create a future that immediately resolves to an error.
        pub fn error(error: E) -> Self {
            Self {
                state: MaybeErrorFutureState::Error(Some(error)),
                response: PhantomData,
            }
        }
    }

    impl<F, R, E> Future for MaybeErrorFuture<F, R, E>
    where
        F: Future<Output = Result<R, E>>,
    {
        type Output = Result<R, E>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> Poll<Self::Output> {
            let mut this = self.project();

            match this.state.as_mut().project() {
                MaybeErrorFutureStateProj::Inner(inner) => inner.poll(cx),
                MaybeErrorFutureStateProj::Error(error) => {
                    Poll::Ready(Err(error.take().expect("polled after error")))
                }
            }
        }
    }
}
