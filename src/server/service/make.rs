use super::http::HttpService;
use http_body::Body as HttpBody;
use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::task::{Context, Poll};
use tower::Service;

pub trait Sealed<Conn> {}

/// A trait for types that can be used to make HTTP services, by recieving references to connections.
pub trait MakeServiceRef<Target, ReqBody>: Sealed<(Target, ReqBody)> {
    /// The `HttpBody` body of the `http::Response`.
    type ResBody: HttpBody;

    /// The error type that can occur within this `Service`.
    type Error: Into<Box<dyn StdError + Send + Sync>>;

    /// The Service type produced to handle requests.
    type Service: HttpService<ReqBody, ResBody = Self::ResBody, Error = Self::Error>;

    /// The error type that occurs if we can't create the service.
    type MakeError: Into<Box<dyn StdError + Send + Sync>>;

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
