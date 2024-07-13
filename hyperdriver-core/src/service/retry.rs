use tower::retry::Policy;

pub use tower::retry::{Retry, RetryLayer};

use crate::body::TryCloneRequest;

/// A policy for retrying requests.
#[derive(Debug, Clone)]
pub struct Attempts(usize);

impl Attempts {
    /// Create a new policy that will retry a request `attempts` times.
    pub fn new(attempts: usize) -> Self {
        Self(attempts)
    }
}

impl<E, BIn, BOut> Policy<http::Request<BIn>, http::Response<BOut>, E> for Attempts
where
    http::Request<BIn>: TryCloneRequest,
{
    type Future = std::future::Ready<Self>;

    fn retry(
        &self,
        _req: &http::Request<BIn>,
        result: Result<&http::Response<BOut>, &E>,
    ) -> Option<Self::Future> {
        match result {
            Ok(res) if res.status().is_server_error() => Some(std::future::ready(Self(self.0 - 1))),
            Ok(_) => None,
            Err(_) if self.0 > 0 => Some(std::future::ready(Self(self.0 - 1))),
            Err(_) => None,
        }
    }

    fn clone_request(&self, req: &http::Request<BIn>) -> Option<http::Request<BIn>> {
        req.try_clone_request()
    }
}
