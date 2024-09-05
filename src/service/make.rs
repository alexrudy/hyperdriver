use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll};

use super::http::HttpService;
use http_body::Body as HttpBody;
use tower::Service;

use tower::{layer::layer_fn, Layer, ServiceExt};

use crate::BoxError;

pub trait Sealed<Conn> {}

/// A trait for types that can be used to make HTTP services, by recieving references to connections.
pub trait MakeServiceRef<Target, ReqBody>: Sealed<(Target, ReqBody)> {
    /// The `HttpBody` body of the `http::Response`.
    type ResBody: HttpBody;

    /// The error type that can occur within this `Service`.
    type Error: Into<BoxError>;

    /// The Service type produced to handle requests.
    type Service: HttpService<ReqBody, ResBody = Self::ResBody, Error = Self::Error>;

    /// The error type that occurs if we can't create the service.
    type MakeError: Into<BoxError>;

    /// The `Future` returned by this `MakeService`.
    type Future: Future<Output = Result<Self::Service, Self::MakeError>>;

    /// Poll the readiness of the make_serivce.
    fn poll_ready_ref(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::MakeError>>;

    /// Create a new service.
    fn make_service_ref(&mut self, target: &Target) -> Self::Future;
}

impl<T, Target, ME, S, F, IB> Sealed<(Target, IB)> for T where
    T: for<'a> Service<&'a Target, Error = ME, Response = S, Future = F>
{
}

impl<T, Target, E, ME, S, F, IB, OB> MakeServiceRef<Target, IB> for T
where
    T: for<'a> Service<&'a Target, Error = ME, Response = S, Future = F>,
    E: Into<Box<dyn StdError + Send + Sync>>,
    ME: Into<Box<dyn StdError + Send + Sync>>,
    S: HttpService<IB, ResBody = OB, Error = E>,
    F: Future<Output = Result<S, ME>>,
    IB: HttpBody,
    OB: HttpBody,
{
    type Error = E;
    type Service = S;
    type ResBody = OB;
    type MakeError = ME;
    type Future = F;

    fn poll_ready_ref(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::MakeError>> {
        self.poll_ready(cx)
    }

    fn make_service_ref(&mut self, target: &Target) -> Self::Future {
        self.call(target)
    }
}

/// Create a `MakeService` from a function.
///
/// # Example
///
/// ```
/// # #[cfg(feature = "runtime")]
/// # async fn run() {
/// use std::convert::Infallible;
/// use hyperdriver::body::{Body, Request, Response};
/// use hyperdriver::server::Server;
/// use hyperdriver::server::service::{make_service_fn, service_fn};
///
/// let addr = ([127, 0, 0, 1], 3000).into();
///
/// let make_svc = make_service_fn(|socket: &AddrStream| {
///     let remote_addr = socket.remote_addr();
///     async move {
///         Ok::<_, Infallible>(service_fn(move |_: Request<Body>| async move {
///             Ok::<_, Infallible>(
///                 Response::new(Body::from(format!("Hello, {}!", remote_addr)))
///             )
///         }))
///     }
/// });
///
/// // Then bind and serve...
/// let server = Server::bind(&addr)
///     .serve(make_svc);
///
/// // Finally, spawn `server` onto an Executor...
/// if let Err(e) = server.await {
///     eprintln!("server error: {}", e);
/// }
/// # }
/// # fn main() {}
/// ```
pub fn make_service_fn<F, Target, Ret>(f: F) -> MakeServiceFn<F>
where
    F: FnMut(&Target) -> Ret,
    Ret: Future,
{
    MakeServiceFn { f }
}

/// `MakeService` returned from [`make_service_fn`]
#[derive(Clone, Copy)]
pub struct MakeServiceFn<F> {
    f: F,
}

impl<'t, F, Ret, Target, Svc, MkErr> Service<&'t Target> for MakeServiceFn<F>
where
    F: FnMut(&Target) -> Ret,
    Ret: Future<Output = Result<Svc, MkErr>>,
    MkErr: Into<Box<dyn StdError + Send + Sync>>,
{
    type Error = MkErr;
    type Response = Svc;
    type Future = Ret;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, target: &'t Target) -> Self::Future {
        (self.f)(target)
    }
}

impl<F> fmt::Debug for MakeServiceFn<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MakeServiceFn").finish()
    }
}

//TODO: Should this really be 'static?
type BoxFuture<T, E> = crate::BoxFuture<'static, Result<T, E>>;
type ServiceRef<T, S, E> =
    dyn for<'a> tower::Service<&'a T, Response = S, Error = E, Future = BoxFuture<S, E>> + Send;

/// A boxed `ServiceRef`.
pub struct BoxMakeServiceRef<Target, Service, MakeServiceError> {
    inner: Box<ServiceRef<Target, Service, MakeServiceError>>,
}

impl<T, S, E> fmt::Debug for BoxMakeServiceRef<T, S, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoxMakeServiceRef").finish()
    }
}

impl<T, S, E> BoxMakeServiceRef<T, S, E> {
    /// Create a new `BoxMakeServiceRef`.
    pub fn new<U, F>(inner: U) -> Self
    where
        U: for<'a> Service<&'a T, Response = S, Error = E, Future = F> + Send + 'static,
        F: Future<Output = Result<S, E>> + Send + 'static,
        T: 'static,
    {
        let inner = Box::new(inner.map_future(|f| Box::pin(f) as _));
        Self { inner }
    }
}

impl<T, S, E> Service<&T> for BoxMakeServiceRef<T, S, E> {
    type Response = S;
    type Error = E;
    type Future = BoxFuture<S, E>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, target: &T) -> Self::Future {
        self.inner.call(target)
    }
}

/// A [`Layer`] that wraps an inner [`MakeServiceRef`] and returns a boxed [`MakeServiceRef`].
pub struct BoxMakeServiceLayer<InnerMakeService, Target, InnerService, MakeServiceError> {
    boxed: Arc<
        dyn Layer<
                InnerMakeService,
                Service = BoxMakeServiceRef<Target, InnerService, MakeServiceError>,
            > + Send
            + Sync
            + 'static,
    >,
}

impl<In, T, U, E> BoxMakeServiceLayer<In, T, U, E> {
    /// Create a new [`BoxMakeServiceLayer`].
    pub fn new<L, F>(inner_layer: L) -> Self
    where
        L: Layer<In> + Send + Sync + 'static,
        L::Service: for<'a> Service<&'a T, Response = U, Error = E, Future = F> + Send + 'static,
        F: Future<Output = Result<U, E>> + Send + 'static,
        T: 'static,
    {
        let layer = layer_fn(move |inner: In| {
            let out = inner_layer.layer(inner);
            BoxMakeServiceRef::new(out)
        });

        Self {
            boxed: Arc::new(layer),
        }
    }
}

impl<In, T, U, E> Layer<In> for BoxMakeServiceLayer<In, T, U, E> {
    type Service = BoxMakeServiceRef<T, U, E>;

    fn layer(&self, inner: In) -> Self::Service {
        self.boxed.layer(inner)
    }
}

impl<In, T, U, E> Clone for BoxMakeServiceLayer<In, T, U, E> {
    fn clone(&self) -> Self {
        Self {
            boxed: Arc::clone(&self.boxed),
        }
    }
}

impl<In, T, U, E> fmt::Debug for BoxMakeServiceLayer<In, T, U, E> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("BoxMakeServiceLayer").finish()
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use static_assertions::assert_impl_all;

    assert_impl_all!(BoxMakeServiceLayer<(), (), (), ()>: Clone, Send, Sync);
    assert_impl_all!(BoxMakeServiceRef<(), (), ()>: Send);
}
