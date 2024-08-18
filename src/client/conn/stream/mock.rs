//! Mock stream connection for testing.
//!
//! Mock streams have no internal functionality, but can be used to test
//! places where a connection is none-the-less required.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;

use tracing::trace;

use crate::client::pool::{PoolableConnection, PoolableTransport};
use crate::info::HasConnectionInfo;

#[cfg(feature = "tls")]
pub use self::tls::MockTls;

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

impl PoolableTransport for MockStream {
    fn can_share(&self) -> bool {
        self.reuse
    }
}

/// A mock connection for testing.
#[derive(Debug)]
pub struct MockConnection {
    stream: MockStream,
}

impl MockConnection {
    /// Create a new mock connection.
    pub fn new(stream: MockStream) -> Self {
        Self { stream }
    }

    /// Close the connection.
    pub fn close(&self) {
        self.stream.close();
    }

    /// Get the unique ID for this connection stream.
    pub fn id(&self) -> StreamID {
        self.stream.id()
    }

    /// Create a new single-use mock connection.
    pub fn single() -> Self {
        Self::new(MockStream::single())
    }

    /// Create a new reusable mock connection.
    pub fn reusable() -> Self {
        Self::new(MockStream::reusable())
    }
}

impl PoolableConnection for MockConnection {
    fn is_open(&self) -> bool {
        self.stream.open.load(Ordering::SeqCst)
    }

    fn can_share(&self) -> bool {
        self.stream.reuse
    }

    fn reuse(&mut self) -> Option<Self> {
        if self.stream.reuse {
            Some(Self::new(self.stream.clone()))
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

    fn info(&self) -> crate::info::ConnectionInfo<Self::Addr> {
        crate::info::ConnectionInfo::default()
    }
}

#[cfg(feature = "tls")]
mod tls {
    use crate::{
        info::{HasConnectionInfo, TlsConnectionInfo},
        stream::tls::TlsHandshakeStream,
    };

    /// Wraps a stream with TLS for testing, with a no-op handshake.
    #[derive(Debug)]
    #[pin_project::pin_project]
    pub struct MockTls<IO> {
        #[pin]
        inner: IO,
        info: crate::info::TlsConnectionInfo,
    }

    impl<IO> MockTls<IO> {
        /// Create a new mock TLS stream.
        pub fn new(inner: IO, info: TlsConnectionInfo) -> Self {
            Self { inner, info }
        }
    }

    impl<IO> std::ops::Deref for MockTls<IO> {
        type Target = IO;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    impl<IO> std::ops::DerefMut for MockTls<IO> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.inner
        }
    }

    impl<IO> TlsHandshakeStream for MockTls<IO>
    where
        IO: TlsHandshakeStream,
    {
        fn poll_handshake(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), std::io::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    impl<IO> HasConnectionInfo for MockTls<IO>
    where
        IO: HasConnectionInfo,
    {
        type Addr = IO::Addr;

        fn info(&self) -> crate::info::ConnectionInfo<Self::Addr> {
            self.inner.info()
        }
    }

    impl<IO> crate::info::HasTlsConnectionInfo for MockTls<IO>
    where
        IO: HasConnectionInfo,
    {
        fn tls_info(&self) -> Option<&TlsConnectionInfo> {
            Some(&self.info)
        }
    }

    impl<IO> tokio::io::AsyncRead for MockTls<IO>
    where
        IO: tokio::io::AsyncRead,
    {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            self.project().inner.poll_read(cx, buf)
        }
    }

    impl<IO> tokio::io::AsyncWrite for MockTls<IO>
    where
        IO: tokio::io::AsyncWrite,
    {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<Result<usize, std::io::Error>> {
            self.project().inner.poll_write(cx, buf)
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), std::io::Error>> {
            self.project().inner.poll_flush(cx)
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), std::io::Error>> {
            self.project().inner.poll_shutdown(cx)
        }
    }
}

#[cfg(test)]
mod tests {

    use static_assertions::assert_impl_all;

    use super::*;

    #[test]
    fn verify_stream() {
        let conn = MockStream::new(false);
        assert!(!conn.can_share());

        let conn = MockStream::new(false);
        conn.close();

        assert_eq!(conn.info().remote_addr(), &MockAddress);

        let dbg = format!("{:?}", conn);
        assert!(dbg.starts_with("MockStream { "));
    }

    assert_impl_all!(MockStream: HasConnectionInfo<Addr=MockAddress>, Clone, fmt::Debug);
}
