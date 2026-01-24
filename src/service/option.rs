use tower::ServiceBuilder;
use tower::layer::util::Stack;

/// Extends `ServiceBuilder` with an `optional` method.
pub trait OptionLayerExt<S> {
    /// Apply an optional middleware to the service, with a consistent concrete error type.
    fn optional<T>(self, middleware: Option<T>) -> ServiceBuilder<Stack<OptionLayer<T>, S>>;
}

impl<S> OptionLayerExt<S> for ServiceBuilder<S> {
    fn optional<T>(self, middleware: Option<T>) -> ServiceBuilder<Stack<OptionLayer<T>, S>> {
        self.layer(OptionLayer::new(middleware))
    }
}

/// A middleware for an optional layer.
#[derive(Debug, Clone)]
pub struct OptionLayer<M> {
    middleware: Option<M>,
}

impl<M> OptionLayer<M> {
    /// Create a new `OptionLayer` with the provided middleware layer.
    pub fn new(middleware: Option<M>) -> Self {
        Self { middleware }
    }
}

impl<M, S> tower::Layer<S> for OptionLayer<M>
where
    M: tower::Layer<S>,
{
    type Service = OptionService<M::Service, S>;

    fn layer(&self, inner: S) -> Self::Service {
        match self.middleware {
            Some(ref middleware) => OptionService {
                inner: OptionServiceState::Middleware(middleware.layer(inner)),
            },
            None => OptionService {
                inner: OptionServiceState::Service(inner),
            },
        }
    }
}

#[derive(Debug, Clone)]
enum OptionServiceState<M, S> {
    Middleware(M),
    Service(S),
}

/// A middleware for an optional layer with merged error types.
///
/// The `tower::util::Either` middleware converts errors to `BoxError`, which
/// can make it difficult to return to a concrete error type. This middleware
/// allows the inner service to return a concrete error type, while the optional
/// middleware must return an error type that can be converted to the inner
/// service's error type.
#[derive(Clone, Debug)]
pub struct OptionService<M, S> {
    inner: OptionServiceState<M, S>,
}

impl<M, S, Req, E> tower::Service<Req> for OptionService<M, S>
where
    S: tower::Service<Req, Error = E>,
    M: tower::Service<Req, Response = S::Response>,
    M::Error: Into<E>,
{
    type Response = S::Response;
    type Error = E;
    type Future = self::future::OptionServiceFuture<M::Future, S::Future>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        match self.inner {
            OptionServiceState::Middleware(ref mut m) => m.poll_ready(cx).map_err(Into::into),
            OptionServiceState::Service(ref mut s) => s.poll_ready(cx),
        }
    }

    fn call(&mut self, req: Req) -> Self::Future {
        match self.inner {
            OptionServiceState::Middleware(ref mut m) => {
                self::future::OptionServiceFuture::middleware(m.call(req))
            }
            OptionServiceState::Service(ref mut s) => {
                self::future::OptionServiceFuture::service(s.call(req))
            }
        }
    }
}

mod future {
    use std::future::Future;

    use pin_project::pin_project;

    #[derive(Debug)]
    #[pin_project(project = StateProj)]
    enum State<M, S> {
        Middleware(#[pin] M),
        Service(#[pin] S),
    }

    #[derive(Debug)]
    #[pin_project]
    pub struct OptionServiceFuture<M, S> {
        #[pin]
        state: State<M, S>,
    }

    impl<M, S> OptionServiceFuture<M, S> {
        pub(super) fn service(inner: S) -> Self {
            Self {
                state: State::Service(inner),
            }
        }

        pub(super) fn middleware(inner: M) -> Self {
            Self {
                state: State::Middleware(inner),
            }
        }
    }

    impl<M, S, R, E, ME> Future for OptionServiceFuture<M, S>
    where
        M: Future<Output = Result<R, ME>>,
        S: Future<Output = Result<R, E>>,
        ME: Into<E>,
    {
        type Output = Result<R, E>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            match self.project().state.project() {
                StateProj::Middleware(future) => {
                    future.poll(cx).map(|result| result.map_err(Into::into))
                }
                StateProj::Service(future) => future.poll(cx),
            }
        }
    }
}
