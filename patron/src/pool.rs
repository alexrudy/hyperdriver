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

use futures_util::future::BoxFuture;
use futures_util::FutureExt as _;
use pin_project::{pin_project, pinned_drop};
use thiserror::Error;
use tokio::sync::oneshot::{Receiver, Sender};
use tracing::debug;
use tracing::instrument::Instrumented;
use tracing::trace;
use tracing::Instrument;

use crate::lazy;

// TODO: Pool should model transport & protocol stages separately, so that it can intervene
// and check for re-usability of the protocol before the handshake starts. If the protocol
// is re-usable, and we start a handshake, we should be able to have additional connections
// wait on the re-usable protocol handshake.

#[cfg(debug_assertions)]
static CHECKOUT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

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

impl<C: PoolableConnection> Pool<C> {
    #[tracing::instrument(skip_all, fields(key = %key), level="debug")]
    pub(crate) fn checkout<F, R, H, T, E>(
        &self,
        key: Key,
        multiplex: bool,
        connect: F,
        handshake: H,
    ) -> Instrumented<Checkout<C, T, E>>
    where
        T: PoolableTransport,
        H: FnOnce(T) -> BoxFuture<'static, Result<C, E>> + Send + 'static,
        F: FnOnce() -> R + Send + 'static,
        R: Future<Output = Result<T, E>> + Send + 'static,
    {
        let mut inner = self.inner.lock().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut connector: Option<(F, H)> = Some((connect, handshake));

        if let Some(connection) = inner.pop(&key) {
            trace!(%key, "connection found in pool");
            connector = None;
            return Checkout::new(key, Some(&self.inner), rx, connector, Some(connection))
                .in_current_span();
        }

        inner.waiting.entry(key.clone()).or_default().push_back(tx);

        if inner.connecting.contains(&key) {
            trace!(%key, "connection in progress, will wait");
            connector = None;
            Checkout::new(key, Some(&self.inner), rx, connector, None).in_current_span()
        } else {
            if multiplex {
                // Only block new connection attempts if we can multiplex on this one.
                inner.connecting.insert(key.clone());
            }
            trace!(%key, "connecting to host");
            Checkout::new(key, Some(&self.inner), rx, connector, None).in_current_span()
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
            trace!(?key, "pending connection cancelled");
        }
        self.waiting.remove(key);
    }

    /// Mark a connection as connected, but not done with the handshake.
    ///
    /// New connection attempts will wait for this connection to complete the
    /// handshake and re-use it if possible.
    fn connected_in_handshake(&mut self, key: &Key) {
        self.connecting.insert(key.clone());
    }
}

impl<T: PoolableConnection> PoolInner<T> {
    fn push(&mut self, key: Key, mut connection: T) {
        self.connecting.remove(&key);

        if let Some(waiters) = self.waiting.get_mut(&key) {
            while let Some(waiter) = waiters.pop_front() {
                if waiter.is_closed() {
                    continue;
                }

                if let Some(conn) = connection.reuse() {
                    let _ = waiter.send(conn);
                } else {
                    tracing::trace!("connection not re-usable, but will be sent to waiter");
                    let _ = waiter.send(connection);
                    return;
                }
            }
        }

        let idle = Idle::new(connection);
        self.idle.entry(key).or_default().push(idle);
    }

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

        if empty && !idle_entry.as_ref().map(|i| i.can_share()).unwrap_or(false) {
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

pub trait PoolableTransport: Unpin + Send + Sized + 'static {
    /// Returns `true` if the transport can be re-used, usually
    /// because it has used ALPN to negotiate a protocol that can
    /// be multiplexed.
    fn can_share(&self) -> bool;
}

pub trait PoolableConnection: Unpin + Send + Sized + 'static {
    fn is_open(&self) -> bool;
    fn can_share(&self) -> bool;
    fn reuse(&mut self) -> Option<Self>;
}

pub(crate) struct Pooled<T: PoolableConnection> {
    connection: Option<T>,
    is_reused: bool,
    key: Key,
    pool: WeakOpt<Mutex<PoolInner<T>>>,
}

impl<T: fmt::Debug + PoolableConnection> fmt::Debug for Pooled<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Pooled").field(&self.connection).finish()
    }
}

impl<T: PoolableConnection> Deref for Pooled<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.connection.as_ref().unwrap()
    }
}

impl<T: PoolableConnection> DerefMut for Pooled<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.connection.as_mut().unwrap()
    }
}

impl<T: PoolableConnection> Drop for Pooled<T> {
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
    Connecting(#[source] E),
    Handshaking(#[source] E),
}

#[derive(Debug)]
#[pin_project(project = WaitingProjected)]
enum Waiting<C> {
    Idle(#[pin] Receiver<C>),
    Connecting(#[pin] Receiver<C>),
}

enum WaitingPoll<C> {
    Connected(C),
    Closed,
    NotReady,
}

impl<C> Future for Waiting<C> {
    type Output = WaitingPoll<C>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project() {
            WaitingProjected::Idle(rx) => match rx.poll(cx) {
                Poll::Ready(Ok(connection)) => Poll::Ready(WaitingPoll::Connected(connection)),
                Poll::Ready(Err(_)) => Poll::Ready(WaitingPoll::Closed),
                Poll::Pending => Poll::Ready(WaitingPoll::NotReady),
            },
            WaitingProjected::Connecting(rx) => match rx.poll(cx) {
                Poll::Ready(Ok(connection)) => Poll::Ready(WaitingPoll::Connected(connection)),
                Poll::Ready(Err(_)) => Poll::Ready(WaitingPoll::Closed),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

enum InnerCheckoutConnecting<C: PoolableConnection, T: PoolableTransport, E> {
    Waiting,
    Connected,
    Connecting {
        connect: BoxFuture<'static, Result<T, E>>,
        handshake: Box<dyn FnOnce(T) -> BoxFuture<'static, Result<C, E>> + Send>,
    },
    Handshaking(BoxFuture<'static, Result<C, E>>),
}

#[pin_project(PinnedDrop)]
pub(crate) struct Checkout<C: PoolableConnection, T: PoolableTransport, E> {
    key: Key,
    pool: WeakOpt<Mutex<PoolInner<C>>>,
    #[pin]
    waiter: Waiting<C>,
    inner: InnerCheckoutConnecting<C, T, E>,
    connection: Option<C>,
    connection_error: PhantomData<E>,
    #[cfg(debug_assertions)]
    id: usize,
}

impl<C: PoolableConnection, T: PoolableTransport, E> Checkout<C, T, E> {
    fn new<R, F, H>(
        key: Key,
        pool: Option<&Arc<Mutex<PoolInner<C>>>>,
        waiter: Receiver<C>,
        connect: Option<(F, H)>,
        connection: Option<C>,
    ) -> Self
    where
        H: FnOnce(T) -> BoxFuture<'static, Result<C, E>> + Send + 'static,
        R: Future<Output = Result<T, E>> + Send + 'static,
        F: FnOnce() -> R + Send + 'static,
    {
        #[cfg(debug_assertions)]
        let id = CHECKOUT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let pool = if let Some(pool) = pool {
            WeakOpt::downgrade(pool)
        } else {
            WeakOpt::none()
        };
        if connection.is_some() {
            debug!(key=%key, "connection recieved from pool");
            Self {
                key,
                pool,
                waiter: Waiting::Idle(waiter),
                inner: InnerCheckoutConnecting::Connected,
                connection,
                connection_error: PhantomData,
                #[cfg(debug_assertions)]
                id,
            }
        } else if let Some((connect, handshake)) = connect {
            Self {
                key,
                pool,
                waiter: Waiting::Idle(waiter),
                inner: InnerCheckoutConnecting::Connecting {
                    connect: Box::pin(lazy::lazy(connect)),
                    handshake: Box::new(handshake),
                },
                connection,
                connection_error: PhantomData,
                #[cfg(debug_assertions)]
                id,
            }
        } else {
            Self {
                key,
                pool,
                waiter: Waiting::Connecting(waiter),
                inner: InnerCheckoutConnecting::Waiting,
                connection,
                connection_error: PhantomData,
                #[cfg(debug_assertions)]
                id,
            }
        }
    }
}

impl<C, T, E> Future for Checkout<C, T, E>
where
    C: PoolableConnection,
    T: PoolableTransport,
    E: 'static,
{
    type Output = Result<Pooled<C>, Error<E>>;

    #[cfg_attr(debug_assertions, tracing::instrument(skip_all, fields(id=%self.id), level="trace"))]
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Outcomes from .poll_waiter:
        // - Ready(Some(connection)) => return connection
        // - Ready(None) => continue to check pool, we don't have a waiter.
        // - Pending => wait on the waiter to complete, don't bother to check pool.

        // Open questions: Should we check the pool for a different connection when the
        // waiter is pending? Probably not, ideally our semantics should keep the pool
        // from containing multiple connections if they can be multiplexed.

        if let WaitingPoll::Connected(connection) = ready!(self.as_mut().poll_waiter(cx)) {
            debug!(key=%self.key, "connection recieved from waiter");
            return Poll::Ready(Ok(connection));
        }

        trace!(key=%self.key, "polling for new connection");
        // Try to connect while we also wait for a checkout to be ready.
        loop {
            let this = self.as_mut().project();
            let connection: T;
            match this.inner {
                InnerCheckoutConnecting::Waiting => {
                    // We're waiting on a connection to be ready.
                    return Poll::Pending;
                }
                InnerCheckoutConnecting::Connected => {
                    // We've already connected, we can just return the connection.
                    let connection = this
                        .connection
                        .take()
                        .expect("future was polled after completion");

                    return Poll::Ready(Ok(self.as_mut().connected(connection)));
                }
                InnerCheckoutConnecting::Connecting { connect, .. } => {
                    connection = ready!(connect.poll_unpin(cx)).map_err(Error::Connecting)?;
                }
                InnerCheckoutConnecting::Handshaking(handshake) => {
                    let connection =
                        ready!(handshake.poll_unpin(cx)).map_err(Error::Handshaking)?;
                    return Poll::Ready(Ok(self.as_mut().connected(connection)));
                }
            }

            if connection.can_share() {
                tracing::trace!(key=%this.key, "connection can be shared");
                if let Some(pool) = this.pool.upgrade() {
                    if let Ok(mut inner) = pool.lock() {
                        inner.connected_in_handshake(&this.key);
                    }
                }
            }

            let InnerCheckoutConnecting::Connecting {
                connect: _,
                handshake,
            } = std::mem::replace(this.inner, InnerCheckoutConnecting::Waiting)
            else {
                unreachable!()
            };

            *this.inner = InnerCheckoutConnecting::Handshaking(Box::pin(handshake(connection)));
        }
    }
}

impl<C: PoolableConnection, T: PoolableTransport, E> Checkout<C, T, E> {
    /// Checks the waiter to see if a new connection is ready and can be passed along.
    ///
    /// If there is no waiter, this function returns `Poll::Ready(Ok(None))`. If there is
    /// a waiter, then it will poll the waiter and return the result.
    fn poll_waiter(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<WaitingPoll<Pooled<C>>> {
        let this = self.project();

        trace!(key=%this.key, "polling for waiter");
        match this.waiter.poll(cx) {
            Poll::Ready(WaitingPoll::Connected(connection)) => {
                if connection.is_open() {
                    Poll::Ready(WaitingPoll::Connected(Pooled {
                        connection: Some(connection),
                        is_reused: true,
                        key: this.key.clone(),
                        pool: this.pool.clone(),
                    }))
                } else {
                    panic!("connection was closed before being returned to the pool");
                }
            }
            Poll::Ready(WaitingPoll::Closed) => Poll::Ready(WaitingPoll::Closed),
            Poll::Ready(WaitingPoll::NotReady) => Poll::Ready(WaitingPoll::NotReady),
            Poll::Pending => Poll::Pending,
        }
    }

    /// Called to register a new connection with the pool.
    fn connected(self: Pin<&mut Self>, mut connection: C) -> Pooled<C> {
        if let Some(pool) = self.pool.upgrade() {
            if let Ok(mut inner) = pool.lock() {
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
impl<C: PoolableConnection, T: PoolableTransport, E> PinnedDrop for Checkout<C, T, E> {
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

    struct MockTransport {
        reuse: bool,
    }

    impl PoolableTransport for MockTransport {
        fn can_share(&self) -> bool {
            self.reuse
        }
    }

    impl MockTransport {
        fn new(reuse: bool) -> Self {
            Self { reuse }
        }

        async fn single() -> Result<Self, Infallible> {
            Ok(Self::new(false))
        }

        async fn reusable() -> Result<Self, Infallible> {
            Ok(Self::new(true))
        }

        fn handshake(self) -> BoxFuture<'static, Result<MockConnection, Infallible>> {
            let reuse = self.reuse;
            Box::pin(async move { Ok(MockConnection::new(reuse)) })
        }
    }

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

        fn close(&self) {
            self.open.store(false, Ordering::SeqCst);
        }
    }

    impl PoolableConnection for MockConnection {
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
            .checkout(
                key.clone(),
                false,
                move || MockTransport::single(),
                MockTransport::handshake,
            )
            .await
            .unwrap();

        assert!(conn.is_open());
        let cid = conn.id();
        drop(conn);

        let conn = pool
            .checkout(
                key.clone(),
                false,
                move || MockTransport::single(),
                MockTransport::handshake,
            )
            .await
            .unwrap();

        assert!(conn.is_open());
        assert_eq!(conn.id(), cid, "connection should be re-used");
        conn.close();
        drop(conn);

        let c2 = pool
            .checkout(
                key,
                false,
                move || MockTransport::single(),
                MockTransport::handshake,
            )
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
            .checkout(
                key.clone(),
                true,
                move || MockTransport::reusable(),
                MockTransport::handshake,
            )
            .await
            .unwrap();

        assert!(conn.is_open());
        let cid = conn.id();
        drop(conn);

        let conn = pool
            .checkout(
                key.clone(),
                true,
                move || MockTransport::reusable(),
                MockTransport::handshake,
            )
            .await
            .unwrap();

        assert!(conn.is_open());
        assert_eq!(conn.id(), cid, "connection should be re-used");
        conn.close();
        drop(conn);

        let conn = pool
            .checkout(
                key.clone(),
                true,
                move || MockTransport::reusable(),
                MockTransport::handshake,
            )
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

        let mut checkout_a = std::pin::pin!(pool.checkout(
            key.clone(),
            true,
            move || async { Ok(rx.await.expect("rx closed")) },
            MockTransport::handshake
        ));

        assert!(futures_util::poll!(&mut checkout_a).is_pending());

        let mut checkout_b = std::pin::pin!(pool.checkout(
            key.clone(),
            true,
            move || MockTransport::reusable(),
            MockTransport::handshake
        ));

        assert!(futures_util::poll!(&mut checkout_b).is_pending());
        assert!(tx.send(MockTransport::reusable().await.unwrap()).is_ok());
        assert!(futures_util::poll!(&mut checkout_b).is_pending());

        let conn_a = checkout_a.await.unwrap();
        assert!(conn_a.is_open());

        let conn_b = checkout_b.await.unwrap();
        assert!(conn_b.is_open());
        assert_eq!(conn_b.id(), conn_a.id(), "connection should be re-used");
    }
}
