//! Connection Pooling for Clients
//!
//! The `pool` module provides a connection pool for clients, which allows for multiple connections to be made to a
//! remote host and reused across multiple requests. This is supported in the `ClientService` type.
//!
//! This connection pool is specifically designed with HTTP connections in mind. It separates the treatment of the
//! connection (e.g. HTTP/1.1, HTTP/2, etc) from the transport (e.g. TCP, TCP+TLS, etc). This allows the pool to be used
//! with any type of connection, as long as it implements the `PoolableConnection` trait, and any type of transport,
//! as long as it implements the `PoolableTransport` trait. This also allows the pool to be used with upgradeable
//! connections, such as HTTP/1.1 connections that can be upgraded to HTTP/2, where the pool will have new HTTP/2
//! connections wait for in-progress upgrades from HTTP/1.1 connections to complete and use those, rather than creating
//! new connections.
//!
//! Pool configuration happens in the `Config` type, which allows for setting the maximum idle duration of a connection,
//! and the maximum number of idle connections per host.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::fmt;
use std::ops::Deref;
use std::ops::DerefMut;
use std::sync::Arc;

use std::time::Duration;

use parking_lot::ArcMutexGuard;
use parking_lot::Mutex;
use tokio::sync::oneshot::Sender;
use tracing::trace;

mod checkout;
mod idle;
pub(super) mod key;
pub(super) mod service;
mod weakopt;

pub(crate) use self::checkout::Checkout;
pub(crate) use self::checkout::Connector;
pub(crate) use self::checkout::Error;
use self::idle::IdleConnections;
pub(crate) use self::key::Key;
use self::weakopt::WeakOpt;

use super::conn::Protocol;
use super::conn::Transport;

/// A pool of connections to remote hosts.
///
/// The pool makes use of a `Checkout` to represent a connection that is being checked out of the pool. The `Checkout`
/// type requires a `Connector` to be provided, which provides a future that will create a new connection to the remote
/// host, and a future that will perform the handshake for the connection. The `Checkout` ensures that in-progress
/// connection state is correctly managed, and that duplicate connections are not made unnecessarily.
///
/// The pool also provides a `Pooled` type, which is a wrapper around a connection that will return the connection to
/// the pool when dropped, if the connection is still open and has not been marked as reusable (reusable connections
/// are always kept in the pool - there is no need to return dropped copies).
#[derive(Debug)]
pub(crate) struct Pool<T: PoolableConnection> {
    inner: Arc<Mutex<PoolInner<T>>>,
}

impl<T: PoolableConnection> Clone for Pool<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T: PoolableConnection> Pool<T> {
    pub(crate) fn new(config: Config) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PoolInner::new(config))),
        }
    }

    fn as_ref(&self) -> PoolRef<T> {
        PoolRef {
            inner: WeakOpt::downgrade(&self.inner),
        }
    }
}

impl<T: PoolableConnection> Default for Pool<T> {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

impl<C: PoolableConnection> Pool<C> {
    /// Create a checkout for a connection to the given key (host/port pair).
    ///
    /// The checkout has several potential behaviors:
    ///
    /// 1. A connection already exists in the pool. It will be immediately available when
    /// polling the checkout object.
    /// 2. A connection is pending (probably in the handshake phase). The checkout will wait
    /// for the pending connection to be ready and re-used.
    /// 3. The checkout will create a new connection if none is available.
    /// 4. During the connection phase, if a new connection is returned to the pool, it will be returned
    /// in place of this one. If `continue_after_preemtion` is `true` in the pool config, the in-progress
    /// connection will continue in the background and be returned to the pool on completion.
    #[cfg_attr(not(tarpaulin), tracing::instrument(skip_all, fields(key = %key), level="debug"))]
    pub(crate) fn checkout<T, P, B>(
        &self,
        key: key::Key,
        multiplex: bool,
        connector: Connector<T, P, B>,
    ) -> Checkout<T, P, B>
    where
        T: Transport,
        P: Protocol<T::IO, B, Connection = C> + Send + 'static,
        C: PoolableConnection,
        B: 'static,
    {
        let mut inner = self.inner.lock();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut connector: Option<Connector<T, P, B>> = Some(connector);

        if let Some(connection) = inner.pop(&key) {
            trace!("connection found in pool");
            connector = None;
            return Checkout::new(key, self, rx, connector, Some(connection), &inner.config);
        }

        trace!("checkout interested in pooled connections");
        inner.waiting.entry(key.clone()).or_default().push_back(tx);

        if inner.connecting.contains(&key) {
            trace!("connection in progress elsewhere, will wait");
            connector = None;
            Checkout::new(key, self, rx, connector, None, &inner.config)
        } else {
            if multiplex {
                // Only block new connection attempts if we can multiplex on this one.
                trace!("checkout of multiplexed connection, other connections should wait");
                inner.connecting.insert(key.clone());
            }
            trace!("connecting to host");
            Checkout::new(key, self, rx, connector, None, &inner.config)
        }
    }
}

struct PoolRef<C>
where
    C: PoolableConnection,
{
    inner: WeakOpt<Mutex<PoolInner<C>>>,
}

impl<C> fmt::Debug for PoolRef<C>
where
    C: PoolableConnection,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PoolRef").field(&self.inner).finish()
    }
}

impl<C> PoolRef<C>
where
    C: PoolableConnection,
{
    fn none() -> Self {
        Self {
            inner: WeakOpt::none(),
        }
    }

    #[allow(dead_code)]
    fn try_lock(&self) -> Option<PoolGuard<C>> {
        self.inner
            .upgrade()
            .and_then(|inner| inner.try_lock_arc().map(PoolGuard))
    }

    fn lock(&self) -> Option<PoolGuard<C>> {
        self.inner
            .upgrade()
            .map(|inner| PoolGuard(inner.lock_arc()))
    }

    #[allow(dead_code)]
    fn is_none(&self) -> bool {
        self.inner.is_none()
    }
}

impl<C> Clone for PoolRef<C>
where
    C: PoolableConnection,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

struct PoolGuard<C: PoolableConnection>(ArcMutexGuard<parking_lot::RawMutex, PoolInner<C>>);

impl<C> Deref for PoolGuard<C>
where
    C: PoolableConnection,
{
    type Target = PoolInner<C>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<C> DerefMut for PoolGuard<C>
where
    C: PoolableConnection,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Debug)]
struct PoolInner<C: PoolableConnection> {
    config: Config,

    connecting: HashSet<key::Key>,
    waiting: HashMap<key::Key, VecDeque<Sender<Pooled<C>>>>,

    idle: HashMap<key::Key, IdleConnections<C>>,
}

impl<C: PoolableConnection> PoolInner<C> {
    fn new(config: Config) -> Self {
        Self {
            config,
            connecting: HashSet::new(),
            waiting: HashMap::new(),
            idle: HashMap::new(),
        }
    }

    fn cancel_connection(&mut self, key: &key::Key) {
        let existed = self.connecting.remove(key);
        if existed {
            trace!(%key, "pending connection cancelled");
        }
    }

    /// Mark a connection as connected, but not done with the handshake.
    ///
    /// New connection attempts will wait for this connection to complete the
    /// handshake and re-use it if possible.
    fn connected_in_handshake(&mut self, key: &key::Key) {
        self.connecting.insert(key.clone());
    }
}

impl<C: PoolableConnection> PoolInner<C> {
    fn push(&mut self, key: key::Key, mut connection: C, pool_ref: PoolRef<C>) {
        self.connecting.remove(&key);

        if let Some(waiters) = self.waiting.get_mut(&key) {
            trace!(waiters=%waiters.len(), "walking waiters");

            while let Some(waiter) = waiters.pop_front() {
                if waiter.is_closed() {
                    trace!("skipping closed waiter");
                    continue;
                }

                if let Some(conn) = connection.reuse() {
                    trace!("re-usable connection will be sent to waiter");
                    let pooled = Pooled {
                        connection: Some(conn),
                        is_reused: true,
                        key: key.clone(),
                        pool: pool_ref.clone(),
                    };

                    if waiter.send(pooled).is_err() {
                        trace!("waiter closed, skipping");
                        continue;
                    };
                } else {
                    trace!("connection not re-usable, but will be sent to waiter");
                    let pooled = Pooled {
                        connection: Some(connection),
                        is_reused: false,
                        key: key.clone(),
                        pool: pool_ref.clone(),
                    };

                    let Err(pooled) = waiter.send(pooled) else {
                        trace!("connection sent");
                        return;
                    };

                    trace!("waiter closed, continuing");
                    connection = pooled.take().unwrap();
                }
            }
        }

        self.idle.entry(key).or_default().push(connection);
    }

    fn pop(&mut self, key: &key::Key) -> Option<C> {
        let mut empty = false;
        let mut idle_entry = None;

        tracing::trace!(%key, "pop");

        if let Some(idle) = self.idle.get_mut(key) {
            idle_entry = idle.pop(self.config.idle_timeout);
            empty = idle.is_empty();
        }

        if empty && !idle_entry.as_ref().map(|i| i.can_share()).unwrap_or(false) {
            trace!(%key, "removing empty idle list");
            self.idle.remove(key);
        }

        idle_entry
    }
}

/// Configuration for a connection pool.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
    /// The maximum idle duration of a connection.
    pub idle_timeout: Option<Duration>,

    /// The maximum number of idle connections per host.
    pub max_idle_per_host: usize,

    /// Should in-progress connections continue after they get pre-empted by a new connection?
    pub continue_after_preemption: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            idle_timeout: Some(Duration::from_secs(90)),
            max_idle_per_host: 32,
            continue_after_preemption: true,
        }
    }
}

/// A [`crate::client::conn::Transport`] Stream that can produce connections
/// which might be poolable.
///
/// This trait is used by the pool connection checkout process before
/// the handshake occurs to check if the connection has negotiated or
/// upgraded to a protocol which enables multiplexing. This is an
/// optimistic check, and the connection will be checked again after
/// the handshake is complete.
pub trait PoolableStream: Unpin + Send + Sized + 'static {
    /// Returns `true` if the transport can be re-used, usually
    /// because it has used ALPN to negotiate a protocol that can
    /// be multiplexed.
    ///
    /// This is effectively speculative, so should only return `true`
    /// when we are sure that the connection on this transport
    /// will be able to multiplex.
    fn can_share(&self) -> bool;
}

/// A [`crate::client::conn::Connection`] that can be pooled.
///
/// These connections must report to the pool whether they remain open,
/// and whether they can be shared / multiplexed.
///
/// The pool will call [`PoolableConnection::reuse`] to get a new connection
/// to return to the pool, which will multiplex against this one. If multiplexing
/// is not possible, then `None` should be returned.
pub trait PoolableConnection: Unpin + Send + Sized + 'static {
    /// Returns `true` if the connection is open.
    fn is_open(&self) -> bool;

    /// Returns `true` if the connection can be shared / multiplexed.
    ///
    /// If this returns `true`, then [`PoolableConnection::reuse`] will be called to get
    /// a new connection to return to the pool.
    fn can_share(&self) -> bool;

    /// Returns a new connection to return to the pool, which will multiplex
    /// against this one if possible.
    fn reuse(&mut self) -> Option<Self>;
}

/// Wrapper type for a connection which is managed by a pool.
///
/// This type is used outside of the Pool to ensure that dropped
/// connections are returned to the pool. The underlying connection
/// is available via `Deref` and `DerefMut`.
pub struct Pooled<C: PoolableConnection> {
    connection: Option<C>,
    is_reused: bool,
    key: key::Key,
    pool: PoolRef<C>,
}

impl<C: PoolableConnection> Pooled<C> {
    fn take(mut self) -> Option<C> {
        self.connection.take()
    }
}

impl<C: fmt::Debug + PoolableConnection> fmt::Debug for Pooled<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Pooled").field(&self.connection).finish()
    }
}

impl<C: PoolableConnection> Deref for Pooled<C> {
    type Target = C;

    fn deref(&self) -> &Self::Target {
        self.connection
            .as_ref()
            .expect("connection only taken on Drop")
    }
}

impl<C: PoolableConnection> DerefMut for Pooled<C> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.connection
            .as_mut()
            .expect("connection only taken on Drop")
    }
}

impl<C: PoolableConnection> Drop for Pooled<C> {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            if connection.is_open() && !self.is_reused {
                if let Some(mut pool) = self.pool.lock() {
                    trace!(key=%self.key, "open connection returned to pool");
                    pool.push(self.key.clone(), connection, self.pool.clone());
                }
            }
        }
    }
}

#[cfg(all(test, feature = "mocks"))]
mod tests {

    use futures_util::FutureExt as _;
    use static_assertions::assert_impl_all;

    use crate::client::conn::protocol::HttpProtocol;
    use crate::client::conn::transport::mock::MockConnectionError;

    use super::*;
    use crate::client::conn::protocol::mock::MockSender;
    use crate::client::conn::stream::mock::MockStream;
    use crate::client::conn::transport::mock::MockTransport;

    #[test]
    fn sensible_config() {
        let _ = tracing_subscriber::fmt::try_init();

        let config = Config::default();
        let pool: Pool<MockSender> = Pool::new(config);

        assert!(pool.inner.lock().config.idle_timeout.unwrap() > Duration::from_secs(1));
        assert!(pool.inner.lock().config.max_idle_per_host > 0);
        assert!(pool.inner.lock().config.max_idle_per_host < 2048);
    }

    assert_impl_all!(Pool<MockSender>: Clone);

    #[tokio::test]
    async fn checkout_simple() {
        let _ = tracing_subscriber::fmt::try_init();

        let pool = Pool::new(Config {
            idle_timeout: Some(Duration::from_secs(10)),
            max_idle_per_host: 5,
            continue_after_preemption: false,
        });

        let key: key::Key = (
            http::uri::Scheme::HTTP,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let conn = pool
            .checkout(
                key.clone(),
                false,
                MockTransport::single()
                    .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
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
                MockTransport::single()
                    .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
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
                MockTransport::single()
                    .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
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
            continue_after_preemption: false,
        });

        let key: key::Key = (
            http::uri::Scheme::HTTPS,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let conn = pool
            .checkout(
                key.clone(),
                true,
                MockTransport::reusable()
                    .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
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
                MockTransport::reusable()
                    .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
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
                MockTransport::reusable()
                    .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
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
            continue_after_preemption: false,
        });

        let key: key::Key = (
            http::uri::Scheme::HTTPS,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let (tx, rx) = tokio::sync::oneshot::channel();

        let mut checkout_a = std::pin::pin!(pool.checkout(
            key.clone(),
            true,
            MockTransport::channel(rx)
                .connector("mock://address".parse().unwrap(), HttpProtocol::Http1)
        ));

        assert!(futures_util::poll!(&mut checkout_a).is_pending());

        let mut checkout_b = std::pin::pin!(pool.checkout(
            key.clone(),
            true,
            MockTransport::reusable()
                .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
        ));

        assert!(futures_util::poll!(&mut checkout_b).is_pending());
        assert!(tx.send(MockStream::reusable()).is_ok());
        assert!(futures_util::poll!(&mut checkout_b).is_pending());

        let conn_a = checkout_a.await.unwrap();
        assert!(conn_a.is_open());

        let conn_b = checkout_b.await.unwrap();
        assert!(conn_b.is_open());
        assert_eq!(conn_b.id(), conn_a.id(), "connection should be re-used");
    }

    #[tokio::test]
    async fn checkout_idle_returned() {
        let _ = tracing_subscriber::fmt::try_init();

        let pool = Pool::new(Config {
            idle_timeout: Some(Duration::from_secs(10)),
            max_idle_per_host: 5,
            continue_after_preemption: false,
        });

        let key: key::Key = (
            http::uri::Scheme::HTTPS,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let conn = MockSender::single();

        let first_id = conn.id();

        let checkout = pool.checkout(
            key.clone(),
            false,
            MockTransport::single()
                .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
        );

        // Return the connection to the pool, sending it out to the new checkout
        // that is waiting, cancelling the checkout connect.

        pool.inner.lock().push(key.clone(), conn, pool.as_ref());

        let conn = checkout.now_or_never().unwrap().unwrap();

        assert!(conn.is_open());
        assert_eq!(conn.id(), first_id, "connection should be re-used");
    }

    #[tokio::test]
    async fn checkout_idle_connected() {
        let _ = tracing_subscriber::fmt::try_init();

        let pool = Pool::new(Config {
            idle_timeout: Some(Duration::from_secs(10)),
            max_idle_per_host: 5,
            continue_after_preemption: false,
        });

        let key: key::Key = (
            http::uri::Scheme::HTTPS,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let conn_first = MockSender::single();

        let first_id = conn_first.id();

        tracing::debug!("Checkout from pool");

        let checkout = pool.checkout(
            key.clone(),
            false,
            MockTransport::single()
                .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
        );

        tracing::debug!("Checking interest");

        // At least one connection should be happening / waiting.
        assert!(!pool
            .inner
            .lock()
            .waiting
            .get(&key)
            .expect("no waiting connections in pool")
            .is_empty());

        tracing::debug!("Resolving checkout");

        let conn = checkout.now_or_never().unwrap().unwrap();

        tracing::debug!("Inserting original connection");
        // Return the connection to the pool, sending it out to the new checkout
        // that is waiting, cancelling the checkout connect.
        pool.inner
            .lock()
            .push(key.clone(), conn_first, pool.as_ref());

        assert!(conn.is_open());
        assert_ne!(conn.id(), first_id, "connection should not be re-used");
    }

    #[tokio::test]
    async fn checkout_drop_pool_err() {
        let _ = tracing_subscriber::fmt::try_init();

        let pool = Pool::new(Config {
            idle_timeout: Some(Duration::from_secs(10)),
            max_idle_per_host: 5,
            continue_after_preemption: false,
        });

        let key: key::Key = (
            http::uri::Scheme::HTTPS,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let start = pool.checkout(
            key.clone(),
            true,
            MockTransport::reusable()
                .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
        );

        let checkout = pool.checkout(
            key.clone(),
            true,
            MockTransport::reusable()
                .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
        );

        drop(start);
        drop(pool);

        assert!(checkout.now_or_never().unwrap().is_err());
    }

    #[tokio::test]
    async fn checkout_drop_pool() {
        let _ = tracing_subscriber::fmt::try_init();

        let pool = Pool::new(Config {
            idle_timeout: Some(Duration::from_secs(10)),
            max_idle_per_host: 5,
            continue_after_preemption: false,
        });

        let key: key::Key = (
            http::uri::Scheme::HTTPS,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let checkout = pool.checkout(
            key.clone(),
            true,
            MockTransport::reusable()
                .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
        );

        drop(pool);

        assert!(checkout.now_or_never().unwrap().is_ok());
    }

    #[tokio::test]
    async fn checkout_connection_error() {
        let _ = tracing_subscriber::fmt::try_init();

        let pool = Pool::new(Config {
            idle_timeout: Some(Duration::from_secs(10)),
            max_idle_per_host: 5,
            continue_after_preemption: false,
        });

        let key: key::Key = (
            http::uri::Scheme::HTTPS,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let checkout = pool.checkout(
            key.clone(),
            true,
            MockTransport::error()
                .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
        );

        let outcome = checkout.now_or_never().unwrap();
        let error = outcome.unwrap_err();
        assert!(matches!(error, Error::Connecting(MockConnectionError)));
    }

    #[tokio::test]
    async fn checkout_simple_cloned() {
        let _ = tracing_subscriber::fmt::try_init();

        let pool = Pool::new(Config {
            idle_timeout: Some(Duration::from_secs(10)),
            max_idle_per_host: 5,
            continue_after_preemption: false,
        });
        let other = pool.clone();

        let key: key::Key = (
            http::uri::Scheme::HTTP,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let conn = pool
            .checkout(
                key.clone(),
                false,
                MockTransport::single()
                    .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
            )
            .await
            .unwrap();

        assert!(conn.is_open());
        let cid = conn.id();
        drop(conn);

        let conn = other
            .checkout(
                key.clone(),
                false,
                MockTransport::single()
                    .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
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
                MockTransport::single()
                    .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
            )
            .await
            .unwrap();

        assert!(c2.is_open());
        assert_ne!(c2.id(), cid, "connection should not be re-used");
    }

    #[tokio::test]
    async fn checkout_delayed_drop() {
        let _ = tracing_subscriber::fmt::try_init();

        let pool = Pool::new(Config {
            idle_timeout: Some(Duration::from_secs(10)),
            max_idle_per_host: 5,
            continue_after_preemption: true,
        });

        let key: key::Key = (
            http::uri::Scheme::HTTP,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let conn = pool
            .checkout(
                key.clone(),
                false,
                MockTransport::single()
                    .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
            )
            .await
            .unwrap();

        assert!(conn.is_open());
        let cid = conn.id();

        let checkout = pool.checkout(
            key.clone(),
            false,
            MockTransport::single()
                .connector("mock://address".parse().unwrap(), HttpProtocol::Http1),
        );

        drop(conn);
        let conn = checkout.await.unwrap();
        assert!(conn.is_open());
        assert_eq!(cid, conn.id());

        let inner = pool.inner.lock();
        let idles = inner.idle.get(&key).unwrap();
        assert_eq!(idles.len(), 1);
    }
}
