//! Happy Eyeballs algorithm for attempting a set of futures in parallel.
//!
//! This module provides a set of utilities for resolving a set of futures in parallel,
//! with a delay between each attempt. The first successful future is returned.
//!
//! The utilities provided here run futures concurrently, but do not spawn them on the
//! runtime.

use std::collections::VecDeque;
use std::future::IntoFuture;
use std::{fmt, future::Future, marker::PhantomData, time::Duration};

use futures_core::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use futures_util::StreamExt;
use tokio::time::error::Elapsed;
use tracing::trace;

/// Error returned when the happy eyeballs algorithm finishes.
///
/// It contains the inner error if an underlying future errored
/// (this will always be the first error)
///
/// Otherwsie, the enum indicates what went wrong.
#[non_exhaustive]
#[derive(Debug, PartialEq, Eq)]
pub enum HappyEyeballsError<T> {
    /// No progress can be made.
    NoProgress,
    /// Timeout reached.
    Timeout(Elapsed),
    /// An error occurred during the underlying future.
    Error(T),
}

impl<T> fmt::Display for HappyEyeballsError<T>
where
    T: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoProgress => write!(f, "no progress can be made"),
            Self::Timeout(e) => write!(f, "timeout: {}", e),
            Self::Error(e) => write!(f, "error: {}", e),
        }
    }
}

impl<T> From<Elapsed> for HappyEyeballsError<T> {
    fn from(e: Elapsed) -> Self {
        Self::Timeout(e)
    }
}

impl<T> std::error::Error for HappyEyeballsError<T>
where
    T: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Timeout(e) => Some(e),
            Self::Error(e) => Some(e),
            _ => None,
        }
    }
}

type HappyEyeballsResult<T, E> = Result<T, HappyEyeballsError<E>>;

/// Implements the Happy Eyeballs algorithm for connecting to a set of addresses.
///
/// This algorithm is used to connect to a set of addresses in parallel, with a
/// delay between each attempt. The first successful connection is returned.
///
/// When the `timeout` is not set, the algorithm will attempt to connect to only
/// one address at a time.
///
/// To connect to all addresses simultaneously, set the `timeout` to zero.
#[derive(Debug)]
pub struct EyeballSet<F, T, E> {
    queue: VecDeque<F>,
    tasks: FuturesUnordered<F>,
    timeout: Option<Duration>,
    initial_concurrency: Option<usize>,
    error: Option<HappyEyeballsError<E>>,
    result: PhantomData<T>,
}

impl<F, T, E> EyeballSet<F, T, E> {
    /// Create a new `EyeballSet` with an optional timeout.
    ///
    /// The timeout is the amount of time between individual connection attempts.
    pub fn new(timeout: Option<Duration>) -> Self {
        Self {
            queue: VecDeque::new(),
            tasks: FuturesUnordered::new(),
            timeout,
            initial_concurrency: None,
            error: None,
            result: PhantomData,
        }
    }

    /// Returns `true` if the set of tasks is empty.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty() && self.queue.is_empty()
    }

    /// Returns the number of tasks in the set.
    pub fn len(&self) -> usize {
        self.tasks.len() + self.queue.len()
    }

    /// Push a future into the set of tasks.
    pub fn push(&mut self, future: F)
    where
        F: Future<Output = std::result::Result<T, E>>,
    {
        self.queue.push_back(future);
    }
}

enum Eyeball<T> {
    Ok(T),
    Error,
    Timeout(Elapsed),
    Exhausted,
}

impl<F, T, E> EyeballSet<F, T, E>
where
    F: Future<Output = Result<T, E>>,
{
    async fn join_next(&mut self) -> Eyeball<T> {
        match self.tasks.next().await {
            Some(Ok(stream)) => Eyeball::Ok(stream),
            Some(Err(e)) if self.error.is_none() => {
                trace!("first attempt error");
                self.error = Some(HappyEyeballsError::Error(e));
                Eyeball::Error
            }
            Some(Err(_)) => {
                trace!("attempt error");
                Eyeball::Error
            }
            None => {
                trace!("exhausted attempts");
                Eyeball::Exhausted
            }
        }
    }

    async fn join_next_with_timeout(&mut self) -> Eyeball<T> {
        if let Some(timeout) = self.timeout {
            match tokio::time::timeout(timeout, self.join_next()).await {
                Ok(outcome) => outcome,
                Err(elapsed) => Eyeball::Timeout(elapsed),
            }
        } else {
            self.join_next().await
        }
    }

    /// Finish the happy eyeballs algorithm, returning the first successful connection.
    pub async fn finish(&mut self) -> HappyEyeballsResult<T, E> {
        for _ in 0..self.initial_concurrency.unwrap_or(self.queue.len()) {
            if let Some(future) = self.queue.pop_front() {
                self.tasks.push(future);
            }
        }

        while let Some(future) = self.queue.pop_front() {
            match self.join_next_with_timeout().await {
                Eyeball::Ok(outcome) => return Ok(outcome),
                _ => self.tasks.push(future),
            }
        }

        loop {
            match self.join_next_with_timeout().await {
                Eyeball::Ok(outcome) => return Ok(outcome),
                Eyeball::Error => continue,
                Eyeball::Timeout(elapsed) => return Err(HappyEyeballsError::Timeout(elapsed)),
                Eyeball::Exhausted => {
                    return self
                        .error
                        .take()
                        .map(Err)
                        .unwrap_or(Err(HappyEyeballsError::NoProgress))
                }
            }
        }
    }
}

impl<F, T, E> IntoFuture for EyeballSet<F, T, E>
where
    T: Send + 'static,
    E: Send + 'static,
    F: Future<Output = Result<T, E>> + Send + 'static,
{
    type Output = HappyEyeballsResult<T, E>;
    type IntoFuture = BoxFuture<'static, Self::Output>;

    fn into_future(mut self) -> Self::IntoFuture {
        Box::pin(async move { self.finish().await })
    }
}

impl<F, T, E> Extend<F> for EyeballSet<F, T, E>
where
    F: Future<Output = Result<T, E>>,
{
    fn extend<I: IntoIterator<Item = F>>(&mut self, iter: I) {
        self.queue.extend(iter);
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

        eyeballs.push(future);

        assert!(!eyeballs.is_empty());

        let result = eyeballs.await;
        assert_eq!(result.unwrap(), 5);
    }

    #[tokio::test]
    async fn one_future_error() {
        let mut eyeballs: EyeballSet<_, (), &str> = EyeballSet::new(Some(Duration::ZERO));

        let future = async { Err::<(), _>("error") };

        eyeballs.push(future);

        let result = eyeballs.await;
        assert!(matches!(
            result.unwrap_err(),
            HappyEyeballsError::Error("error")
        ));
    }

    #[tokio::test]
    async fn one_future_timeout() {
        let mut eyeballs: EyeballSet<_, (), &str> = EyeballSet::new(Some(Duration::ZERO));

        let future = pending();
        eyeballs.push(future);

        let result = eyeballs.await;
        assert!(matches!(
            result.unwrap_err(),
            HappyEyeballsError::Timeout(_)
        ));
    }

    #[tokio::test]
    async fn empty_set() {
        let eyeballs: EyeballSet<Pending<Result<(), &str>>, (), &str> =
            EyeballSet::new(Some(Duration::ZERO));

        assert!(eyeballs.is_empty());
        let result = eyeballs.await;
        assert!(matches!(
            result.unwrap_err(),
            HappyEyeballsError::NoProgress
        ));
    }

    #[tokio::test]
    async fn multiple_futures_success() {
        let mut eyeballs = EyeballSet::new(Some(Duration::ZERO));

        let future1 = ready(Err::<u32, String>("error".into()));
        let future2 = ready(Ok::<_, String>(5));
        let future3 = ready(Ok::<_, String>(10));

        eyeballs.extend(vec![future1, future2, future3]);
        let result = eyeballs.await;

        assert_eq!(result.unwrap(), 5);
    }

    #[tokio::test]
    async fn multiple_futures_until_finished() {
        let mut eyeballs = EyeballSet::new(Some(Duration::ZERO));

        let future1 = ready(Err::<u32, String>("error".into()));
        let future2 = ready(Ok::<_, String>(5));
        let future3 = ready(Ok::<_, String>(10));

        eyeballs.push(future1);
        eyeballs.push(future2);
        eyeballs.push(future3);

        assert_eq!(eyeballs.len(), 3);

        let result = eyeballs.await;

        assert_eq!(result.unwrap(), 5);
    }

    #[tokio::test]
    async fn multiple_futures_error() {
        let mut eyeballs = EyeballSet::new(Some(Duration::ZERO));

        let future1 = ready(Err::<u32, &str>("error 1"));
        let future2 = ready(Err::<u32, &str>("error 2"));
        let future3 = ready(Err::<u32, &str>("error 3"));

        eyeballs.extend(vec![future1, future2, future3]);
        let result = eyeballs.await;

        assert!(matches!(
            result.unwrap_err(),
            HappyEyeballsError::Error("error 1")
        ));
    }

    #[tokio::test]
    async fn no_timeout() {
        let mut eyeballs = EyeballSet::new(None);

        let future1 = ready(Err::<u32, &str>("error 1"));
        let future2 = ready(Err::<u32, &str>("error 2"));
        let future3 = ready(Err::<u32, &str>("error 3"));

        eyeballs.extend(vec![future1, future2, future3]);

        let result = eyeballs.await;

        assert!(matches!(
            result.unwrap_err(),
            HappyEyeballsError::Error("error 1")
        ));
    }
}
