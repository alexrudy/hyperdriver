use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ops::DerefMut;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use std::task::ready;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use std::time::Instant;

use pin_project::{pin_project, pinned_drop};
use thiserror::Error;
use tokio::sync::oneshot::{Receiver, Sender};
use tracing::instrument::Instrumented;
use tracing::trace;
use tracing::Instrument;

use crate::lazy;
use crate::lazy::Lazy;

struct WeakOpt<T>(Option<Weak<T>>);

impl<T> WeakOpt<T> {
    fn none() -> Self {
        Self(None)
    }

    fn downgrade(arc: &Arc<T>) -> Self {
        Self(Some(Arc::downgrade(arc)))
    }

    fn upgrade(&self) -> Option<Arc<T>> {
        self.0.as_ref().and_then(|weak| weak.upgrade())
    }
}

impl<T> Clone for WeakOpt<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct Key(http::uri::Scheme, http::uri::Authority);

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}", self.0, self.1)
    }
}

impl From<(http::uri::Scheme, http::uri::Authority)> for Key {
    fn from(value: (http::uri::Scheme, http::uri::Authority)) -> Self {
        Self(value.0, value.1)
    }
}

impl From<http::Uri> for Key {
    fn from(value: http::Uri) -> Self {
        let parts = value.into_parts();

        Self(parts.scheme.unwrap(), parts.authority.unwrap())
    }
}

/// A pool of connections to a database.
#[derive(Debug)]
pub(crate) struct Pool<T> {
    inner: Arc<Mutex<PoolInner<T>>>,
}

impl<T> Clone for Pool<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Pool<T> {
    pub(crate) fn new(config: Config) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PoolInner::new(config))),
        }
    }
}

impl<T: Poolable> Pool<T> {
    #[tracing::instrument(skip(self, key, multiplex, connect), fields(key = %key))]
    pub(crate) fn checkout<F, R, E>(
        &self,
        key: Key,
        multiplex: bool,
        connect: F,
    ) -> Instrumented<Checkout<T, F, R, E>>
    where
        F: FnOnce() -> R,
        R: Future<Output = Result<T, E>>,
    {
        if multiplex {
            let mut inner = self.inner.lock().unwrap();
            if inner.connecting.contains(&key) {
                let (tx, rx) = tokio::sync::oneshot::channel();
                inner.waiting.entry(key.clone()).or_default().push_back(tx);

                Checkout {
                    key: key.clone(),
                    pool: WeakOpt::downgrade(&self.inner),
                    waiter: Some(rx),
                    connect: lazy::lazy(connect),
                    connection_error: PhantomData,
                }
                .in_current_span()
            } else {
                inner.connecting.insert(key.clone());

                Checkout {
                    key: key.clone(),
                    pool: WeakOpt::downgrade(&self.inner),
                    waiter: None,
                    connect: lazy::lazy(connect),
                    connection_error: PhantomData,
                }
                .in_current_span()
            }
        } else {
            Checkout {
                key: key.clone(),
                pool: WeakOpt::downgrade(&self.inner),
                waiter: None,
                connect: lazy::lazy(connect),
                connection_error: PhantomData,
            }
            .in_current_span()
        }
    }
}

#[derive(Debug)]
struct PoolInner<T> {
    config: Config,

    connecting: HashSet<Key>,
    waiting: HashMap<Key, VecDeque<Sender<T>>>,

    idle: HashMap<Key, Vec<Idle<T>>>,
}

impl<T> PoolInner<T> {
    fn new(config: Config) -> Self {
        Self {
            config,
            connecting: HashSet::new(),
            waiting: HashMap::new(),
            idle: HashMap::new(),
        }
    }

    fn cancel_connection(&mut self, key: &Key) {
        let existed = self.connecting.remove(key);
        if existed {
            trace!(?key, "connection cancelled");
        }
        self.waiting.remove(key);
    }

    fn push(&mut self, key: Key, connection: T) {
        let idle = Idle::new(connection);
        self.idle.entry(key).or_default().push(idle);
    }
}

impl<T: Poolable> PoolInner<T> {
    fn pop(&mut self, key: &Key) -> Option<T> {
        let mut empty = false;
        let mut idle_entry = None;
        if let Some(idle) = self.idle.get_mut(key) {
            if !idle.is_empty() {
                let exipred = self
                    .config
                    .idle_timeout
                    .filter(|timeout| timeout.as_secs_f64() > 0.0)
                    .and_then(|timeout| {
                        let now: Instant = Instant::now();
                        now.checked_sub(timeout)
                    });

                trace!(%key, "checking {} idle connections", idle.len());

                while let Some(entry) = idle.pop() {
                    if exipred.map(|expired| entry.at < expired).unwrap_or(false) {
                        trace!(%key, "found expired connection");
                        empty = true;
                        break;
                    }

                    if entry.inner.is_open() {
                        trace!(%key, "found idle connection");
                        idle_entry = Some(entry.inner);
                        break;
                    } else {
                        trace!(%key, "found closed connection");
                    }
                }

                empty |= idle.is_empty();
            }
        }

        if empty {
            trace!(%key, "removing empty idle list");
            self.idle.remove(key);
        }

        idle_entry
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub idle_timeout: Option<Duration>,
    pub max_idle_per_host: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            idle_timeout: Some(Duration::from_secs(90)),
            max_idle_per_host: 32,
        }
    }
}

#[derive(Debug)]
struct Idle<T> {
    at: Instant,
    inner: T,
}

impl<T> Idle<T> {
    fn new(inner: T) -> Self {
        Self {
            at: Instant::now(),
            inner,
        }
    }
}

pub trait Poolable: Unpin + Send + Sized + 'static {
    fn is_open(&self) -> bool;
    fn can_share(&self) -> bool;
    fn reuse(&mut self) -> Option<Self>;
}

pub(crate) struct Pooled<T: Poolable> {
    connection: Option<T>,
    is_reused: bool,
    key: Key,
    pool: WeakOpt<Mutex<PoolInner<T>>>,
}

impl<T: Poolable> Deref for Pooled<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.connection.as_ref().unwrap()
    }
}

impl<T: Poolable> DerefMut for Pooled<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.connection.as_mut().unwrap()
    }
}

impl<T: Poolable> fmt::Debug for Pooled<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pooled")
            .field("connection", &self.connection)
            .field("key", &self.key)
            .field("is_reused", &self.is_reused)
            .finish()
    }
}

impl<T: Poolable> Drop for Pooled<T> {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            if connection.is_open() && !self.is_reused {
                if let Some(pool) = self.pool.upgrade() {
                    if let Ok(mut inner) = pool.lock() {
                        trace!(key=%self.key, "open connection returned to pool");
                        inner.push(self.key.clone(), connection);
                    }
                }
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum Error<E> {
    Closed,
    Connecting(#[source] E),
}

#[pin_project(PinnedDrop)]
pub(crate) struct Checkout<T: Poolable, F, R, E> {
    key: Key,
    pool: WeakOpt<Mutex<PoolInner<T>>>,
    waiter: Option<Receiver<T>>,
    #[pin]
    connect: Lazy<F, R>,
    connection_error: PhantomData<E>,
}

impl<T: Poolable, F, R, E> Future for Checkout<T, F, R, E>
where
    F: FnOnce() -> R,
    R: Future<Output = Result<T, E>>,
{
    type Output = Result<Pooled<T>, Error<E>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(connection) = ready!(self.as_mut().poll_waiter(cx)?) {
            trace!(key=%self.key, "connection recieved from waiter");
            return Poll::Ready(Ok(connection));
        }

        if let Some(connection) = self.as_mut().checkout() {
            if connection.is_open() {
                trace!(key=%self.key, "connection recieved from pool");
                return Poll::Ready(Ok(connection));
            }
        }

        // Try to connect while we also wait for a checkout to be ready.
        let this = self.as_mut().project();
        match this.connect.poll(cx) {
            Poll::Ready(Ok(connection)) => {
                trace!(key=%self.key, "connection recieved from connect");
                Poll::Ready(Ok(self.as_mut().connected(connection)))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(Error::Connecting(e))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T: Poolable, F, R, E> Checkout<T, F, R, E>
where
    F: FnOnce() -> R,
    R: Future<Output = Result<T, E>>,
{
    /// Checks the waiter to see if a new connection is ready and can be passed along.
    fn poll_waiter(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<Pooled<T>>, Error<E>>> {
        let this = self.project();
        if let Some(mut rx) = this.waiter.take() {
            match Pin::new(&mut rx).poll(cx) {
                Poll::Ready(Ok(connection)) => {
                    if connection.is_open() {
                        Poll::Ready(Ok(Some(Pooled {
                            connection: Some(connection),
                            is_reused: true,
                            key: this.key.clone(),
                            pool: this.pool.clone(),
                        })))
                    } else {
                        Poll::Ready(Err(Error::Closed))
                    }
                }
                Poll::Ready(Err(_)) => Poll::Ready(Err(Error::Closed)),
                Poll::Pending => {
                    *this.waiter = Some(rx);
                    Poll::Pending
                }
            }
        } else {
            Poll::Ready(Ok(None))
        }
    }

    /// Checks the pool to see if an idle connection is available.
    fn checkout(self: Pin<&mut Self>) -> Option<Pooled<T>> {
        if let Some(pool) = self.pool.upgrade() {
            if let Ok(mut inner) = pool.lock() {
                if let Some(mut connection) = inner.pop(&self.key) {
                    if let Some(reuse) = connection.reuse() {
                        inner.push(self.key.clone(), reuse);
                        return Some(Pooled {
                            connection: Some(connection),
                            is_reused: true,
                            key: self.key.clone(),
                            pool: WeakOpt::none(),
                        });
                    };
                    return Some(Pooled {
                        connection: Some(connection),
                        is_reused: false,
                        key: self.key.clone(),
                        pool: WeakOpt::downgrade(&pool),
                    });
                }
            }
        }
        None
    }

    /// Called to add a connection to a pool
    fn connected(self: Pin<&mut Self>, mut connection: T) -> Pooled<T> {
        if let Some(pool) = self.pool.upgrade() {
            if let Ok(mut inner) = pool.lock() {
                inner.connecting.remove(&self.key);
                if let Some(waiters) = inner.waiting.remove(&self.key) {
                    for waiter in waiters.into_iter().filter(|waiter| !waiter.is_closed()) {
                        if let Some(conn) = connection.reuse() {
                            let _ = waiter.send(conn);
                        } else {
                            panic!("waiter waiting on connection which can't be re-used");
                        }
                    }
                }

                if let Some(reused) = connection.reuse() {
                    inner.push(self.key.clone(), reused);
                    return Pooled {
                        connection: Some(connection),
                        is_reused: true,
                        key: self.key.clone(),
                        pool: WeakOpt::none(),
                    };
                } else {
                    return Pooled {
                        connection: Some(connection),
                        is_reused: false,
                        key: self.key.clone(),
                        pool: WeakOpt::downgrade(&pool),
                    };
                }
            }
        }

        // No pool or lock was available, so we can't add the connection to the pool.
        Pooled {
            connection: Some(connection),
            is_reused: false,
            key: self.key.clone(),
            pool: WeakOpt::none(),
        }
    }
}

#[pinned_drop]
impl<T: Poolable, F, R, E> PinnedDrop for Checkout<T, F, R, E> {
    fn drop(self: Pin<&mut Self>) {
        if let Some(pool) = self.pool.upgrade() {
            if let Ok(mut inner) = pool.lock() {
                inner.cancel_connection(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        sync::{
            atomic::{AtomicBool, AtomicU16, Ordering},
            Arc,
        },
    };

    static IDENT: AtomicU16 = AtomicU16::new(1);

    use super::*;

    struct MockConnection {
        open: Arc<AtomicBool>,
        reuse: bool,
        ident: u16,
    }

    impl MockConnection {
        fn id(&self) -> u16 {
            self.ident
        }

        fn new(reuse: bool) -> Self {
            let conn = Self {
                open: Arc::new(AtomicBool::new(true)),
                reuse,
                ident: IDENT.fetch_add(1, Ordering::SeqCst),
            };
            trace!(id=%conn.id(), "creating connection");
            conn
        }

        async fn single() -> Result<Self, Infallible> {
            Ok(Self::new(false))
        }

        async fn reusable() -> Result<Self, Infallible> {
            Ok(Self::new(true))
        }

        fn close(&self) {
            self.open.store(false, Ordering::SeqCst);
        }
    }

    impl Poolable for MockConnection {
        fn is_open(&self) -> bool {
            self.open.load(Ordering::SeqCst)
        }

        fn can_share(&self) -> bool {
            self.reuse
        }

        fn reuse(&mut self) -> Option<Self> {
            if self.reuse && self.is_open() {
                Some(Self {
                    open: self.open.clone(),
                    reuse: true,
                    ident: self.ident,
                })
            } else {
                None
            }
        }
    }

    #[tokio::test]
    async fn checkout_simple() {
        let _ = tracing_subscriber::fmt::try_init();

        let pool = Pool::new(Config {
            idle_timeout: Some(Duration::from_secs(10)),
            max_idle_per_host: 5,
        });

        let key: Key = (
            http::uri::Scheme::HTTP,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let conn = pool
            .checkout(key.clone(), false, move || MockConnection::single())
            .await
            .unwrap();

        assert!(conn.is_open());
        let cid = conn.id();
        drop(conn);

        let conn = pool
            .checkout(key.clone(), false, move || MockConnection::single())
            .await
            .unwrap();

        assert!(conn.is_open());
        assert_eq!(conn.id(), cid, "connection should be re-used");

        let c2 = pool
            .checkout(key, false, move || MockConnection::single())
            .await
            .unwrap();

        assert!(c2.is_open());
        assert_ne!(c2.id(), cid, "connection should not be re-used");
    }

    #[tokio::test]
    async fn checkout_multiplex() {
        let _ = tracing_subscriber::fmt::try_init();

        let pool = Pool::new(Config {
            idle_timeout: Some(Duration::from_secs(10)),
            max_idle_per_host: 5,
        });

        let key: Key = (
            http::uri::Scheme::HTTPS,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let conn = pool
            .checkout(key.clone(), true, move || MockConnection::reusable())
            .await
            .unwrap();

        assert!(conn.is_open());
        let cid = conn.id();
        drop(conn);

        let conn = pool
            .checkout(key.clone(), true, move || MockConnection::reusable())
            .await
            .unwrap();

        assert!(conn.is_open());
        assert_eq!(conn.id(), cid, "connection should be re-used");
        conn.close();
        drop(conn);

        let conn = pool
            .checkout(key.clone(), true, move || MockConnection::reusable())
            .await
            .unwrap();
        assert!(conn.is_open());
        assert_ne!(conn.id(), cid, "connection should not be re-used");
    }

    #[tokio::test]
    async fn checkout_multiplex_contended() {
        let _ = tracing_subscriber::fmt::try_init();

        let pool = Pool::new(Config {
            idle_timeout: Some(Duration::from_secs(10)),
            max_idle_per_host: 5,
        });

        let key: Key = (
            http::uri::Scheme::HTTPS,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let (tx, rx) = tokio::sync::oneshot::channel();

        let mut checkout_a = std::pin::pin!(pool.checkout(key.clone(), true, move || rx));

        assert!(futures_util::poll!(&mut checkout_a).is_pending());

        let mut checkout_b =
            std::pin::pin!(pool.checkout(key.clone(), true, move || MockConnection::reusable()));

        assert!(futures_util::poll!(&mut checkout_b).is_pending());
        assert!(tx.send(MockConnection::reusable().await.unwrap()).is_ok());
        assert!(futures_util::poll!(&mut checkout_b).is_pending());

        let conn_a = checkout_a.await.unwrap();
        assert!(conn_a.is_open());

        let conn_b = checkout_b.await.unwrap();
        assert!(conn_b.is_open());
        assert_eq!(conn_b.id(), conn_a.id(), "connection should be re-used");
    }
}
