use std::convert::Infallible;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use tracing::trace;

use super::{PoolableConnection, PoolableTransport};

static IDENT: AtomicU16 = AtomicU16::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ConnectionId(u16);

impl ConnectionId {
    pub(super) fn new() -> Self {
        Self(IDENT.fetch_add(1, Ordering::SeqCst))
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "conn-{}", self.0)
    }
}

#[derive(Debug)]
pub(super) struct MockTransport {
    reuse: bool,
}

impl PoolableTransport for MockTransport {
    fn can_share(&self) -> bool {
        self.reuse
    }
}

impl MockTransport {
    pub(super) fn new(reuse: bool) -> Self {
        Self { reuse }
    }

    pub(super) async fn single() -> Result<Self, Infallible> {
        Ok(Self::new(false))
    }

    pub(super) async fn reusable() -> Result<Self, Infallible> {
        Ok(Self::new(true))
    }

    pub(super) fn handshake(self) -> BoxFuture<'static, Result<MockConnection, Infallible>> {
        let reuse = self.reuse;
        Box::pin(async move { Ok(MockConnection::new(reuse)) })
    }
}

#[derive(Debug)]
pub(super) struct MockConnection {
    open: Arc<AtomicBool>,
    reuse: bool,
    ident: ConnectionId,
}

impl MockConnection {
    pub(super) fn id(&self) -> ConnectionId {
        self.ident
    }

    fn new(reuse: bool) -> Self {
        let conn = Self {
            open: Arc::new(AtomicBool::new(true)),
            reuse,
            ident: ConnectionId::new(),
        };
        trace!(id=%conn.id(), "creating connection");
        conn
    }

    pub(super) fn single() -> Self {
        Self::new(false)
    }

    #[allow(dead_code)]
    pub(super) fn reusable() -> Self {
        Self::new(true)
    }

    pub(super) fn close(&self) {
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

#[test]
fn verify_mock() {
    let mut conn = MockConnection::new(false);
    assert!(conn.is_open());
    assert!(!conn.can_share());
    assert!(conn.reuse().is_none());

    let conn = MockConnection::new(false);
    conn.close();
    assert!(!conn.is_open());

    let dbg = format!("{:?}", conn);
    assert!(dbg.starts_with("MockConnection { "));
}
