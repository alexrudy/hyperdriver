//! Mock stream connection for testing.
//!
//! Mock streams have no internal functionality, but can be used to test
//! places where a connection is none-the-less required.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;

use tracing::trace;

use crate::pool::PoolableConnection;
use hyperdriver_core::info::HasConnectionInfo;

static IDENT: AtomicU16 = AtomicU16::new(1);

/// A unique identifier for a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamID(u16);

impl StreamID {
    /// Create a new unique stream identifier.
    pub fn new() -> Self {
        Self(IDENT.fetch_add(1, Ordering::SeqCst))
    }
}

impl Default for StreamID {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for StreamID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stream-{}", self.0)
    }
}

/// A mock stream connection for testing.
#[derive(Debug, Clone)]
pub struct MockStream {
    open: Arc<AtomicBool>,
    reuse: bool,
    ident: StreamID,
}

impl MockStream {
    /// Get a unique ID for this connection stream.
    ///
    /// This is useful for testing that a stream which is re-used is indeed
    /// the same stream.
    pub fn id(&self) -> StreamID {
        self.ident
    }

    /// Create a new mock stream connection.
    pub fn new(reuse: bool) -> Self {
        let conn = Self {
            open: Arc::new(AtomicBool::new(true)),
            reuse,
            ident: StreamID::new(),
        };
        trace!(id=%conn.id(), "creating connection");
        conn
    }

    /// Create a new single-use mock stream connection.
    pub fn single() -> Self {
        Self::new(false)
    }

    /// Create a new reusable mock stream connection.
    pub fn reusable() -> Self {
        Self::new(true)
    }

    /// Close the connection.
    pub fn close(&self) {
        self.open.store(false, Ordering::SeqCst);
    }
}

impl PoolableConnection for MockStream {
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

/// A mock address for testing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MockAddress;

impl fmt::Display for MockAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mock://")
    }
}

impl HasConnectionInfo for MockStream {
    type Addr = MockAddress;

    fn info(&self) -> hyperdriver_core::info::ConnectionInfo<Self::Addr> {
        hyperdriver_core::info::ConnectionInfo::default()
    }
}

#[cfg(test)]
mod tests {

    use static_assertions::assert_impl_all;

    use super::*;

    #[test]
    fn verify_stream() {
        let mut conn = MockStream::new(false);
        assert!(conn.is_open());
        assert!(!conn.can_share());
        assert!(conn.reuse().is_none());

        let conn = MockStream::new(false);
        conn.close();
        assert!(!conn.is_open());

        assert_eq!(conn.info().remote_addr(), &MockAddress);

        let dbg = format!("{:?}", conn);
        assert!(dbg.starts_with("MockStream { "));
    }

    assert_impl_all!(MockStream: HasConnectionInfo<Addr=MockAddress>, Clone, fmt::Debug);
}
