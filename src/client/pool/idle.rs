use std::time::{Duration, Instant};

use tracing::trace;

use super::PoolableConnection;

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

#[derive(Debug)]
pub(super) struct IdleConnections<T> {
    inner: Vec<Idle<T>>,
}

impl<T> Default for IdleConnections<T> {
    fn default() -> Self {
        Self { inner: Vec::new() }
    }
}

impl<T> IdleConnections<T> {
    pub(super) fn push(&mut self, inner: T) {
        self.inner.push(Idle::new(inner));
    }

    pub(super) fn pop(&mut self, idle_timeout: Option<Duration>) -> Option<T>
    where
        T: PoolableConnection,
    {
        let mut empty = false;
        let mut idle_entry = None;

        if !self.is_empty() {
            let exipred = idle_timeout
                .filter(|timeout| timeout.as_secs_f64() > 0.0)
                .and_then(|timeout| {
                    let now: Instant = Instant::now();
                    now.checked_sub(timeout)
                });

            trace!("checking {} idle connections", self.len());

            while let Some(entry) = self.inner.pop() {
                if exipred.map(|expired| entry.at < expired).unwrap_or(false) {
                    trace!("found expired connection");
                    empty = true;
                    break;
                }

                if entry.inner.is_open() {
                    trace!("found idle connection");
                    idle_entry = Some(entry.inner);
                    break;
                } else {
                    trace!("found closed connection");
                }
            }

            empty |= self.is_empty();
        }

        if empty {
            self.clear();
        }

        idle_entry
    }

    pub(super) fn len(&self) -> usize {
        self.inner.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.inner.clear();
    }
}

#[cfg(all(test, feature = "mocks"))]
mod test {
    use std::thread;

    use super::*;
    use crate::client::conn::stream::mock::MockConnection;

    #[test]
    fn verify_idle() {
        let conn = MockConnection::single();
        let idle = Idle::new(conn);

        let dbg = format!("{:?}", idle);
        assert!(dbg.starts_with("Idle { at: Instant {"));
    }

    #[test]
    fn verify_idle_connections() {
        let mut idle = IdleConnections::default();
        assert_eq!(idle.len(), 0);
        assert!(idle.is_empty());

        assert_eq!(format!("{:?}", idle), "IdleConnections { inner: [] }");

        let conn = MockConnection::single();
        idle.push(conn);

        assert_eq!(idle.len(), 1);
        assert!(!idle.is_empty());

        let conn = idle.pop(None);
        assert!(conn.is_some());
        assert_eq!(idle.len(), 0);
        assert!(idle.is_empty());
    }

    #[test]
    fn test_idle_connections() {
        let mut idle = IdleConnections::default();
        assert_eq!(idle.len(), 0);

        let conn1 = MockConnection::single();
        let conn2 = MockConnection::single();
        let conn3 = MockConnection::single();

        idle.push(conn1);
        idle.push(conn2);
        idle.push(conn3);

        assert_eq!(idle.len(), 3);

        let conn = idle.pop(None);
        assert!(conn.is_some());
        assert_eq!(idle.len(), 2);

        let conn = idle.pop(None);
        assert!(conn.is_some());
        assert_eq!(idle.len(), 1);

        let conn = idle.pop(None);
        assert!(conn.is_some());
        assert_eq!(idle.len(), 0);

        let conn = idle.pop(None);
        assert!(conn.is_none());
        assert_eq!(idle.len(), 0);
    }

    #[test]
    fn test_idle_connections_with_timeout() {
        let mut idle = IdleConnections::default();
        assert_eq!(idle.len(), 0);

        let conn1 = MockConnection::single();
        let conn2 = MockConnection::single();
        let conn3 = MockConnection::single();

        idle.push(conn1);
        idle.push(conn2);
        idle.push(conn3);

        assert_eq!(idle.len(), 3);

        let conn = idle.pop(Some(Duration::from_secs(1)));
        assert!(conn.is_some());
        assert_eq!(idle.len(), 2);

        let conn = idle.pop(Some(Duration::from_secs(1)));
        assert!(conn.is_some());
        assert_eq!(idle.len(), 1);

        let conn = idle.pop(Some(Duration::from_secs(1)));
        assert!(conn.is_some());
        assert_eq!(idle.len(), 0);

        let conn = idle.pop(Some(Duration::from_secs(1)));
        assert!(conn.is_none());
        assert_eq!(idle.len(), 0);
    }

    #[test]
    fn test_idle_connections_with_timeout_expired() {
        let mut idle = IdleConnections::default();
        assert_eq!(idle.len(), 0);

        let conn1 = MockConnection::single();
        let conn2 = MockConnection::single();
        let conn3 = MockConnection::single();

        idle.push(conn1);
        idle.push(conn2);
        idle.push(conn3);

        assert_eq!(idle.len(), 3);

        thread::sleep(Duration::from_millis(10));

        let conn = idle.pop(Some(Duration::from_millis(1)));
        assert!(conn.is_none());
        assert_eq!(idle.len(), 0);
    }
}
