use std::fmt;
use std::future::{ready, Ready};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use thiserror::Error;
use tracing::trace;

use crate::client::conn::connection::ConnectionError;
use crate::client::conn::{Connection, ProtocolRequest, TransportStream};
use crate::info::{HasConnectionInfo, TlsConnectionInfo};

use super::{PoolableConnection, PoolableTransport};

static IDENT: AtomicU16 = AtomicU16::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ConnectionId(u16);

impl ConnectionId {
    pub(crate) fn new() -> Self {
        Self(IDENT.fetch_add(1, Ordering::SeqCst))
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "conn-{}", self.0)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("connection error")]
pub(crate) struct MockConnectionError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportMode {
    SingleUse,
    Reusable,
    ConnectionError,
}

#[derive(Debug, Clone)]
pub(crate) struct MockTransport {
    mode: TransportMode,
}

impl PoolableTransport for MockTransport {
    fn can_share(&self) -> bool {
        matches!(self.mode, TransportMode::Reusable)
    }
}

impl MockTransport {
    pub(crate) fn new(reuse: bool) -> Self {
        let mode = if reuse {
            TransportMode::Reusable
        } else {
            TransportMode::SingleUse
        };
        Self { mode }
    }

    pub(crate) fn single() -> Ready<Result<Self, MockConnectionError>> {
        ready(Ok(Self::new(false)))
    }

    pub(crate) fn reusable() -> Ready<Result<Self, MockConnectionError>> {
        ready(Ok(Self::new(true)))
    }

    /// Return a future representing the completion of the connection handshake.
    ///
    /// This must return a boxed future for compatibility with the underlying
    /// pool.
    pub(crate) fn handshake(
        self,
    ) -> BoxFuture<'static, Result<MockConnection, MockConnectionError>> {
        let reuse = matches!(self.mode, TransportMode::Reusable);
        ready(Ok(MockConnection::new(reuse))).boxed()
    }

    pub(crate) fn error() -> Ready<Result<Self, MockConnectionError>> {
        ready(Err(MockConnectionError))
    }

    pub(crate) fn connection_error() -> Self {
        Self {
            mode: TransportMode::ConnectionError,
        }
    }
}

impl tower::Service<http::Uri> for MockTransport {
    type Response = TransportStream<MockConnection>;

    type Error = MockConnectionError;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: http::Uri) -> Self::Future {
        let reuse = match self.mode {
            TransportMode::SingleUse => false,
            TransportMode::Reusable => true,
            TransportMode::ConnectionError => return Box::pin(ready(Err(MockConnectionError))),
        };

        let conn = MockConnection::new(reuse);
        Box::pin(async move {
            Ok(TransportStream::new(
                conn,
                #[cfg(feature = "tls")]
                None,
            ))
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MockConnection {
    open: Arc<AtomicBool>,
    reuse: bool,
    ident: ConnectionId,
}

impl MockConnection {
    pub(crate) fn id(&self) -> ConnectionId {
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

    pub(crate) fn single() -> Self {
        Self::new(false)
    }

    #[allow(dead_code)]
    pub(crate) fn reusable() -> Self {
        Self::new(true)
    }

    pub(crate) fn close(&self) {
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MockAddress;

impl fmt::Display for MockAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mock://somewhere")
    }
}

impl HasConnectionInfo for MockConnection {
    type Addr = MockAddress;

    fn info(&self) -> crate::info::ConnectionInfo<Self::Addr> {
        crate::info::ConnectionInfo::default()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MockSender;

impl Connection for MockSender {
    type ResBody = crate::Body;

    type Error = MockProtocolError;

    type Future = Ready<Result<crate::body::Response, Self::Error>>;

    fn send_request(&mut self, request: crate::body::Request) -> Self::Future {
        ready(Ok(crate::body::Response::new(request.into_body())))
    }

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn version(&self) -> http::Version {
        http::Version::HTTP_11
    }
}

impl PoolableConnection for MockSender {
    fn is_open(&self) -> bool {
        true
    }

    fn can_share(&self) -> bool {
        true
    }

    fn reuse(&mut self) -> Option<Self> {
        Some(self.clone())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("mock protocol error")]
pub(crate) struct MockProtocolError;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MockProtocol {
    tls: Option<TlsConnectionInfo>,
}

impl tower::Service<ProtocolRequest<MockConnection>> for MockProtocol {
    type Response = MockSender;

    type Error = ConnectionError;
    type Future = Ready<Result<MockSender, ConnectionError>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: ProtocolRequest<MockConnection>) -> Self::Future {
        ready(Ok(MockSender))
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
