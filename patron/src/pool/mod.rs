use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::fmt;
use std::ops::Deref;
use std::ops::DerefMut;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::oneshot::Sender;
use tracing::trace;

mod checkout;
mod idle;
mod key;
mod weakopt;

#[cfg(test)]
mod mock;

pub(crate) use self::checkout::Checkout;
pub(crate) use self::checkout::Connector;
pub(crate) use self::checkout::Error;
use self::idle::IdleConnections;
pub(crate) use self::key::Key;
use self::weakopt::WeakOpt;

/// A pool of connections to remote hosts.
///
/// This connection pool is specifically designed with HTTP connections in mind. It separates the treatment of the
/// connection (e.g. HTTP/1.1, HTTP/2, etc) from the transport (e.g. TCP, TLS, etc). This allows the pool to be used
/// with any type of connection, as long as it implements the `PoolableConnection` trait, and any type of transport,
/// as long as it implements the `PoolableTransport` trait. This also allows the pool to be used with upgradeable
/// connections, such as HTTP/1.1 connections that can be upgraded to HTTP/2, where the pool will have new HTTP/2
/// connections wait for in-progress upgrades from HTTP/1.1 connections to complete and use those, rather than creating
/// new connections.
///
/// The pool makes use of a `Checkout` to represent a connection that is being checked out of the pool. The `Checkout`
/// type requires a `Connector` to be provided, which provides a future that will create a new connection to the remote
/// host, and a future that will perform the handshake for the connection. The `Checkout` ensures that in-progress
/// connection state is correctly managed, and that duplicate connections are not made unnecessarily.
///
/// The pool also provides a `Pooled` type, which is a wrapper around a connection that will return the connection to
/// the pool when dropped, if the connection is still open and has not been marked as reusable (reusable connections
/// are always kept in the pool - there is no need to return dropped copies).
///
/// Pool configuration happens in the `Config` type, which allows for setting the maximum idle duration of a connection,
/// and the maximum number of idle connections per host.
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
}

impl<C: PoolableConnection> Pool<C> {
    #[tracing::instrument(skip_all, fields(key = %key), level="debug")]
    pub(crate) fn checkout<T, E>(
        &self,
        key: key::Key,
        multiplex: bool,
        connector: Connector<C, T, E>,
    ) -> Checkout<C, T, E>
    where
        T: PoolableTransport,
    {
        let mut inner = self.inner.lock().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut connector: Option<Connector<C, T, E>> = Some(connector);

        if let Some(connection) = inner.pop(&key) {
            trace!("connection found in pool");
            connector = None;
            return Checkout::new(key, &self.inner, rx, connector, Some(connection));
        }

        trace!("checkout interested in pooled connections");
        inner.waiting.entry(key.clone()).or_default().push_back(tx);

        if inner.connecting.contains(&key) {
            trace!("connection in progress, will wait");
            connector = None;
            Checkout::new(key, &self.inner, rx, connector, None)
        } else {
            if multiplex {
                // Only block new connection attempts if we can multiplex on this one.
                trace!("checkout of multiplexed connection, other connections should wait");
                inner.connecting.insert(key.clone());
            }
            trace!("connecting to host");
            Checkout::new(key, &self.inner, rx, connector, None)
        }
    }
}

#[derive(Debug)]
struct PoolInner<T: PoolableConnection> {
    config: Config,

    connecting: HashSet<key::Key>,
    waiting: HashMap<key::Key, VecDeque<Sender<Pooled<T>>>>,

    idle: HashMap<key::Key, IdleConnections<T>>,
}

impl<T: PoolableConnection> PoolInner<T> {
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

impl<T: PoolableConnection> PoolInner<T> {
    fn push(&mut self, key: key::Key, mut connection: T, pool_ref: WeakOpt<Mutex<Self>>) {
        self.connecting.remove(&key);

        if let Some(waiters) = self.waiting.get_mut(&key) {
            trace!(waiters=%waiters.len(), "Walking waiters");

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

                    let _ = waiter.send(pooled);
                } else {
                    trace!("connection not re-usable, but will be sent to waiter");
                    let pooled = Pooled {
                        connection: Some(connection),
                        is_reused: false,
                        key,
                        pool: pool_ref,
                    };
                    let _ = waiter.send(pooled);
                    return;
                }
            }
        }

        self.idle.entry(key).or_default().push(connection);
    }

    fn pop(&mut self, key: &key::Key) -> Option<T> {
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
pub struct Config {
    /// The maximum idle duration of a connection.
    pub idle_timeout: Option<Duration>,

    /// The maximum number of idle connections per host.
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

/// A [`crate::conn::Transport`] that can be pooled.
pub trait PoolableTransport: Unpin + Send + Sized + 'static {
    /// Returns `true` if the transport can be re-used, usually
    /// because it has used ALPN to negotiate a protocol that can
    /// be multiplexed.
    fn can_share(&self) -> bool;
}

/// A [`crate::Protocol`] that can be pooled.

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

pub(crate) struct Pooled<T: PoolableConnection> {
    connection: Option<T>,
    is_reused: bool,
    key: key::Key,
    pool: weakopt::WeakOpt<Mutex<PoolInner<T>>>,
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
                        inner.push(self.key.clone(), connection, self.pool.clone());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures_util::FutureExt as _;
    use static_assertions::assert_impl_all;

    use super::mock::{MockConnection, MockTransport};
    use super::*;

    #[test]
    fn sensible_config() {
        let _ = tracing_subscriber::fmt::try_init();

        let config = Config::default();
        let pool: Pool<MockConnection> = Pool::new(config);

        assert!(pool.inner.lock().unwrap().config.idle_timeout.unwrap() > Duration::from_secs(1));
        assert!(pool.inner.lock().unwrap().config.max_idle_per_host > 0);
        assert!(pool.inner.lock().unwrap().config.max_idle_per_host < 2048);
    }

    assert_impl_all!(Pool<MockConnection>: Clone);

    #[tokio::test]
    async fn checkout_simple() {
        let _ = tracing_subscriber::fmt::try_init();

        let pool = Pool::new(Config {
            idle_timeout: Some(Duration::from_secs(10)),
            max_idle_per_host: 5,
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
                Connector::new(MockTransport::single, MockTransport::handshake),
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
                Connector::new(MockTransport::single, MockTransport::handshake),
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
                Connector::new(MockTransport::single, MockTransport::handshake),
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

        let key: key::Key = (
            http::uri::Scheme::HTTPS,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let conn = pool
            .checkout(
                key.clone(),
                true,
                Connector::new(MockTransport::reusable, MockTransport::handshake),
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
                Connector::new(MockTransport::reusable, MockTransport::handshake),
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
                Connector::new(MockTransport::reusable, MockTransport::handshake),
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

        let key: key::Key = (
            http::uri::Scheme::HTTPS,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let (tx, rx) = tokio::sync::oneshot::channel();

        let mut checkout_a = std::pin::pin!(pool.checkout(
            key.clone(),
            true,
            Connector::new(
                move || async { Ok(rx.await.expect("rx closed")) },
                MockTransport::handshake
            )
        ));

        assert!(futures_util::poll!(&mut checkout_a).is_pending());

        let mut checkout_b = std::pin::pin!(pool.checkout(
            key.clone(),
            true,
            Connector::new(MockTransport::reusable, MockTransport::handshake)
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

    #[tokio::test]
    async fn checkout_idle_returned() {
        let _ = tracing_subscriber::fmt::try_init();

        let pool = Pool::new(Config {
            idle_timeout: Some(Duration::from_secs(10)),
            max_idle_per_host: 5,
        });

        let key: key::Key = (
            http::uri::Scheme::HTTPS,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let conn = MockConnection::single();

        let first_id = conn.id();

        let checkout = pool.checkout(
            key.clone(),
            false,
            Connector::new(MockTransport::single, MockTransport::handshake),
        );

        // Return the connection to the pool, sending it out to the new checkout
        // that is waiting, cancelling the checkout connect.
        let pool_ref = WeakOpt::downgrade(&pool.inner);

        pool.inner.lock().unwrap().push(key.clone(), conn, pool_ref);

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
        });

        let key: key::Key = (
            http::uri::Scheme::HTTPS,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let conn_first = MockConnection::single();

        let first_id = conn_first.id();

        tracing::debug!("Checkout from pool");

        let checkout = pool.checkout(
            key.clone(),
            false,
            Connector::new(MockTransport::single, MockTransport::handshake),
        );

        tracing::debug!("Checking interest");

        // At least one connection should be happening / waiting.
        assert!(!pool
            .inner
            .lock()
            .unwrap()
            .waiting
            .get(&key)
            .expect("no waiting connections in pool")
            .is_empty());

        tracing::debug!("Resolving checkout");

        let conn = checkout.now_or_never().unwrap().unwrap();

        tracing::debug!("Inserting original connection");
        // Return the connection to the pool, sending it out to the new checkout
        // that is waiting, cancelling the checkout connect.
        let pool_ref = WeakOpt::downgrade(&pool.inner);
        pool.inner
            .lock()
            .unwrap()
            .push(key.clone(), conn_first, pool_ref);

        assert!(conn.is_open());
        assert_ne!(conn.id(), first_id, "connection should not be re-used");
    }

    #[tokio::test]
    async fn checkout_drop_pool_err() {
        let _ = tracing_subscriber::fmt::try_init();

        let pool = Pool::new(Config {
            idle_timeout: Some(Duration::from_secs(10)),
            max_idle_per_host: 5,
        });

        let key: key::Key = (
            http::uri::Scheme::HTTPS,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let start = pool.checkout(
            key.clone(),
            true,
            Connector::new(MockTransport::reusable, MockTransport::handshake),
        );

        let checkout = pool.checkout(
            key.clone(),
            true,
            Connector::new(MockTransport::reusable, MockTransport::handshake),
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
        });

        let key: key::Key = (
            http::uri::Scheme::HTTPS,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();

        let checkout = pool.checkout(
            key.clone(),
            true,
            Connector::new(MockTransport::reusable, MockTransport::handshake),
        );

        drop(pool);

        assert!(checkout.now_or_never().unwrap().is_ok());
    }
}
