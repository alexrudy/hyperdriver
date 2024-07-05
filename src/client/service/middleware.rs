#![allow(dead_code, unused_variables)]

use std::time::Duration;

/// Middleware for a client, which combines retries, follow-redirects and timeouts.
#[derive(Debug)]
pub struct ClientMiddleware<C> {
    inner: C,
    timeout: Duration,
    user_agent: Option<http::HeaderValue>,
}

impl<C, R> tower::Service<R> for ClientMiddleware<C>
where
    C: tower::Service<R>,
{
    type Response = C::Response;

    type Error = crate::client::Error;

    type Future = future::ResponseFuture<C, R>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn call(&mut self, req: R) -> Self::Future {
        todo!()
    }
}

mod future {

    use std::fmt;
    use std::future::Future;
    use std::marker::PhantomData;

    #[pin_project::pin_project]
    pub struct ResponseFuture<C, R>
    where
        C: tower::Service<R>,
    {
        #[pin]
        inner: C::Future,
        request: PhantomData<fn(R) -> ()>,
    }

    impl<C, R> fmt::Debug for ResponseFuture<C, R>
    where
        C: tower::Service<R>,
    {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("ResponseFuture").finish()
        }
    }

    impl<C, R> Future for ResponseFuture<C, R>
    where
        C: tower::Service<R>,
    {
        type Output = Result<C::Response, crate::client::Error>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            todo!()
        }
    }
}
