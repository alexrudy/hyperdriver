use hyper::rt::Executor;
/// A tokio executor for running futures within hyper.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioExecutor;

impl TokioExecutor {
    /// Create a new tokio executor.
    pub fn new() -> Self {
        Self
    }
}

impl<F> Executor<F> for TokioExecutor
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, future: F) {
        tokio::spawn(future);
    }
}
