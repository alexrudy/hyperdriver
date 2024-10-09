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

/// A tokio executor for running futures on the current thread.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioCurrentThreadExecutor;

impl TokioCurrentThreadExecutor {
    /// Create a new tokio executor.
    pub fn new() -> Self {
        Self
    }
}

impl<F> Executor<F> for TokioCurrentThreadExecutor
where
    F: std::future::Future + 'static,
    F::Output: 'static,
{
    fn execute(&self, future: F) {
        tokio::task::spawn_local(future);
    }
}
