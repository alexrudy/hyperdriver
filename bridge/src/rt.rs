use hyper::rt::Executor;

#[derive(Debug, Default, Clone, Copy)]
pub struct TokioExecutor;

impl TokioExecutor {
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
