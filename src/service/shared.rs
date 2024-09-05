use std::fmt;

use crate::BoxFuture;
use tower::{Service, ServiceExt};

/// A [`Service`] that can be cloned, sent, and shared across threads.
pub struct SharedService<T, U, E>(
    Box<
        dyn CloneService<T, Response = U, Error = E, Future = BoxFuture<'static, Result<U, E>>>
            + Send
            + Sync
            + 'static,
    >,
);

impl<T, U, E> SharedService<T, U, E> {
    /// Create a new `SharedService` from a `Service`.
    pub fn new<S>(service: S) -> Self
    where
        S: Service<T, Response = U, Error = E> + Clone + Send + Sync + 'static,
        S::Future: Send + 'static,
    {
        Self(Box::new(service.map_future(|f| Box::pin(f) as _)))
    }

    /// Create a layer which wraps a `Service` in a `SharedService`.
    pub fn layer<S>() -> impl tower::layer::Layer<S, Service = SharedService<T, U, E>>
    where
        S: Service<T, Response = U, Error = E> + Clone + Send + Sync + 'static,
        S::Future: Send + 'static,
    {
        tower::layer::layer_fn(Self::new)
    }
}

impl<T, U, E> Service<T> for SharedService<T, U, E> {
    type Response = U;

    type Error = E;

    type Future = BoxFuture<'static, Result<U, E>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, req: T) -> Self::Future {
        self.0.call(req)
    }
}

impl<T, U, E> Clone for SharedService<T, U, E> {
    fn clone(&self) -> Self {
        Self(self.0.clone_box())
    }
}

trait CloneService<R>: Service<R> {
    fn clone_box(
        &self,
    ) -> Box<
        dyn CloneService<R, Response = Self::Response, Error = Self::Error, Future = Self::Future>
            + Send
            + Sync
            + 'static,
    >;
}

impl<R, T> CloneService<R> for T
where
    T: Service<R> + Clone + Send + Sync + 'static,
{
    fn clone_box(
        &self,
    ) -> Box<
        dyn CloneService<R, Response = T::Response, Error = T::Error, Future = T::Future>
            + Send
            + Sync
            + 'static,
    > {
        Box::new(self.clone())
    }
}

impl<T, U, E> fmt::Debug for SharedService<T, U, E> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("SharedService").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use static_assertions::assert_impl_all;

    assert_impl_all!(SharedService<http::Request<crate::Body>, http::Response<crate::Body>, Box<dyn std::error::Error + Send + Sync + 'static>>: Clone, Send, Sync);
}
