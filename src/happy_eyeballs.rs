use std::{fmt, future::Future, marker::PhantomData, time::Duration};

use futures_util::stream::FuturesUnordered;
use futures_util::StreamExt;
use tracing::trace;

/// Implements the Happy Eyeballs algorithm for connecting to a set of addresses.
///
/// This algorithm is used to connect to a set of addresses in parallel, with a
/// delay between each attempt. The first successful connection is returned.
///
/// When the `timeout` is not set, the algorithm will attempt to connect to only
/// one address at a time.
///
/// To connect to all addresses simultaneously, set the `timeout` to zero.
pub struct EyeballSet<F, T, E> {
    tasks: FuturesUnordered<F>,
    timeout: Option<Duration>,
    error: Option<BoxError>,
    result: PhantomData<Result<T, E>>,
}

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

impl<F, T, E> EyeballSet<F, T, E> {
    /// Create a new `EyeballSet` with an optional timeout.
    ///
    /// The timeout is the amount of time between individual connection attempts.
    pub fn new(timeout: Option<Duration>) -> Self {
        Self {
            tasks: FuturesUnordered::new(),
            timeout,
            error: None,
            result: PhantomData,
        }
    }

    /// Returns `true` if the set of tasks is empty.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Returns the number of tasks in the set.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Spawn a future into the set of tasks.
    pub fn spawn(&mut self, future: F)
    where
        F: Future<Output = Result<T, E>>,
    {
        self.tasks.push(future);
    }
}

impl<F, T, E> EyeballSet<F, T, E>
where
    E: fmt::Display + Into<BoxError>,
    F: Future<Output = Result<T, E>>,
{
    async fn join_next(&mut self) -> Option<Result<T, BoxError>> {
        match self.tasks.next().await {
            Some(Ok(stream)) => {
                // self.tasks.abort_all();
                return Some(Ok(stream));
            }
            Some(Err(e)) if self.error.is_none() => {
                trace!("attempt error: {}", e);
                self.error = Some(e.into());
            }
            Some(Err(e)) => {
                trace!("attempt error: {}", e);
            }
            None => {
                trace!("exhausted attempts");
                // self.tasks.abort_all();
                return self.error.take().map(Err);
            }
        }

        None
    }

    /// Finalize the set of futures, returning the first successful future,
    /// or an error if all futures failed.
    ///
    /// This function will block until a future is resolved. If no future is available,
    /// this function will panic.
    pub async fn finalize(&mut self) -> Result<T, BoxError> {
        if let Some(outcome) = self.next().await {
            outcome
        } else if let Some(error) = self.error.take() {
            trace!("finalizing with error: {}", error);
            Err(error)
        } else {
            Err("timed out".into())
        }
    }

    /// Resolve the next future in the set of tasks.
    ///
    /// This function will return `None` if the timeout is reached, or if a task returns an error.
    /// If a task returns a successful result, that result is returned. If all tasks are exhausted,
    /// the error from the first task is returned.
    pub async fn next(&mut self) -> Option<Result<T, BoxError>> {
        if let Some(timeout) = self.timeout {
            match tokio::time::timeout(timeout, self.join_next()).await {
                Ok(Some(Ok(stream))) => Some(Ok(stream)),
                Ok(Some(Err(e))) => Some(Err(e)),
                Ok(None) => None,
                Err(_) => {
                    tracing::trace!(timeout.ms=%timeout.as_millis(), "happy eyeballs timeout");
                    None
                }
            }
        } else {
            trace!("not using happy eyeballs");
            self.join_next().await
        }
    }

    /// Resolve a set of futures which are triggered with a possible delay.
    ///
    /// This function will resolve the futures in the order they are provided,
    /// with a delay between spawning each future. The first successful future is returned.
    pub async fn from_iterator(
        &mut self,
        iter: impl IntoIterator<Item = F>,
    ) -> Result<T, BoxError> {
        for future in iter.into_iter() {
            self.spawn(future);
            if let Some(outcome) = self.next().await {
                return outcome;
            }
        }

        self.finalize().await
    }

    pub async fn until_finished(&mut self) -> Result<T, BoxError> {
        while let Some(outcome) = self.next().await {
            if let Ok(outcome) = outcome {
                return Ok(outcome);
            }
        }

        self.finalize().await
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::future::ready;
    use std::future::Pending;

    use super::*;

    #[tokio::test]
    async fn one_future_success() {
        let mut eyeballs = EyeballSet::new(Some(Duration::ZERO));

        let future = async { Ok::<_, String>(5) };

        eyeballs.spawn(future);

        let result = eyeballs.finalize().await;
        assert_eq!(result.unwrap(), 5);
    }

    #[tokio::test]
    async fn one_future_error() {
        let mut eyeballs: EyeballSet<_, (), &str> = EyeballSet::new(Some(Duration::ZERO));

        let future = async { Err::<(), _>("error") };

        eyeballs.spawn(future);

        let result = eyeballs.finalize().await;
        assert_eq!(result.unwrap_err().to_string(), "error");
    }

    #[tokio::test]
    async fn one_future_timeout() {
        let mut eyeballs: EyeballSet<_, (), &str> = EyeballSet::new(Some(Duration::ZERO));

        let future = pending();
        eyeballs.spawn(future);

        let result = eyeballs.finalize().await;
        assert_eq!(result.unwrap_err().to_string(), "timed out");
    }

    #[tokio::test]
    async fn empty_set() {
        let mut eyeballs: EyeballSet<Pending<Result<(), &str>>, (), &str> =
            EyeballSet::new(Some(Duration::ZERO));

        let result = eyeballs.finalize().await;
        assert_eq!(result.unwrap_err().to_string(), "timed out");
    }

    #[tokio::test]
    async fn multiple_futures_success() {
        let mut eyeballs = EyeballSet::new(Some(Duration::ZERO));

        let future1 = ready(Err::<u32, String>("error".into()));
        let future2 = ready(Ok::<_, String>(5));
        let future3 = ready(Ok::<_, String>(10));

        let result = eyeballs
            .from_iterator(vec![future1, future2, future3])
            .await;

        assert_eq!(result.unwrap(), 5);
    }

    #[tokio::test]
    async fn multiple_futures_until_finished() {
        let mut eyeballs = EyeballSet::new(Some(Duration::ZERO));

        let future1 = ready(Err::<u32, String>("error".into()));
        let future2 = ready(Ok::<_, String>(5));
        let future3 = ready(Ok::<_, String>(10));

        eyeballs.spawn(future1);
        eyeballs.spawn(future2);
        eyeballs.spawn(future3);

        let result = eyeballs.until_finished().await;

        assert_eq!(result.unwrap(), 5);
    }

    #[tokio::test]
    async fn multiple_futures_error() {
        let mut eyeballs = EyeballSet::new(Some(Duration::ZERO));

        let future1 = ready(Err::<u32, String>("error 1".into()));
        let future2 = ready(Err::<u32, String>("error 2".into()));
        let future3 = ready(Err::<u32, String>("error 3".into()));

        let result = eyeballs
            .from_iterator(vec![future1, future2, future3])
            .await;

        assert_eq!(result.unwrap_err().to_string(), "error 1");
    }

    #[tokio::test]
    async fn no_timeout() {
        let mut eyeballs = EyeballSet::new(None);

        let future1 = ready(Err::<u32, String>("error 1".into()));
        let future2 = ready(Err::<u32, String>("error 2".into()));
        let future3 = ready(Err::<u32, String>("error 3".into()));

        let result = eyeballs
            .from_iterator(vec![future1, future2, future3])
            .await;

        assert_eq!(result.unwrap_err().to_string(), "error 1");
    }
}
