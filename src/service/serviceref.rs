use std::{future::Future, task::Context};

pub trait Sealed<C> {}

/// A [`tower::Service`] which takes a reference to a conenction type,
/// usually in a "make service" context. Effectively this is `Service<&IO>`,
/// but gets around the limitations of an impl with lifetimes.
pub trait ServiceRef<IO>: Sealed<IO> {
    /// The future returned by the service.
    type Future: Future<Output = Result<Self::Response, Self::Error>>;

    /// The response type of the service.
    type Response;

    /// The error type of the service.
    type Error;

    /// Poll the service to determine if it is ready to process requests.
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> std::task::Poll<Result<(), Self::Error>>;

    /// Call the service with a reference to the connection.
    fn call(&mut self, stream: &IO) -> Self::Future;
}

impl<IO, T> Sealed<IO> for T where T: for<'a> tower::Service<&'a IO> {}

impl<IO, T, F, R, E> ServiceRef<IO> for T
where
    T: for<'a> tower::Service<&'a IO, Future = F, Response = R, Error = E>,
    F: Future<Output = Result<R, E>>,
{
    type Future = F;
    type Response = R;
    type Error = E;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        tower::Service::poll_ready(self, cx)
    }

    fn call(&mut self, stream: &IO) -> Self::Future {
        tower::Service::call(self, stream)
    }
}
