//! A duplex stream suitable for a hyper transport mechanism.
//!
//! This isn't just a plain-old stream, becasue we need to support
//! multiple connect/accept pairs to fully support the server.
//!
//! Right now, this is done with a `DuplexClient` and `DuplexIncoming` pair,
//! which produce a new duplex stream for each connection, provided via
//! the `Connect` and `Accept` traits.
//!
//! Using the connect and accept parts manually can be a bit tricky, since
//! the single process must be polling the accept trait in order for the
//! connect trait to suceed.
//!
//! To use these by hand, you will have to poll the accept and the connect futures
//! together, like so:
//! ```
//! # use hyperdriver::stream::duplex::{self, DuplexClient};
//! # use hyperdriver::server::conn::AcceptExt;
//! # async fn demo_duplex() {
//! let (client, incoming) = duplex::pair();
//!
//! let (client_conn, server_conn) = tokio::try_join!(client.connect(1024), incoming.accept()).unwrap();
//!
//! # }
//! ```

use core::fmt;
use std::{
    io,
    pin::Pin,
    sync::atomic::AtomicUsize,
    task::{ready, Context, Poll},
};

use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::info::{self, HasConnectionInfo};

#[cfg(feature = "server")]
use crate::server::conn::Accept;

static IDENTITY: AtomicUsize = AtomicUsize::new(1);

/// Address (blank) for a duplex stream
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct DuplexAddr {
    identity: usize,
}

impl fmt::Debug for DuplexAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DuplexAddr").field(&self.identity).finish()
    }
}

impl DuplexAddr {
    /// Create a new duplex address
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            identity: IDENTITY.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        }
    }
}

impl fmt::Display for DuplexAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "duplex://{}", self.identity)
    }
}

type ConnectionInfo = info::ConnectionInfo<DuplexAddr>;

/// A duplex stream transports data entirely in memory within the tokio runtime.
#[derive(Debug)]
#[pin_project]
pub struct DuplexStream {
    #[pin]
    inner: tokio::io::DuplexStream,
    info: ConnectionInfo,
}

impl HasConnectionInfo for DuplexStream {
    type Addr = DuplexAddr;

    fn info(&self) -> ConnectionInfo {
        self.info.clone()
    }
}

impl DuplexStream {
    /// Create a new duplex stream pair.
    ///
    /// The stream will be created with a buffer, and the `name` and `protocol` will be used to
    /// create the connection info. Normally, this method is not needed, an you should prefer
    /// using [`DuplexClient`] and [`DuplexIncoming`] together to create a client/server pair of duplex streams.
    pub fn new(max_buf_size: usize) -> (Self, Self) {
        let (a, b) = tokio::io::duplex(max_buf_size);
        let info = info::ConnectionInfo::from(DuplexAddr::new());
        (
            DuplexStream {
                inner: a,
                info: info.clone(),
            },
            DuplexStream { inner: b, info },
        )
    }
}

#[cfg(feature = "client")]
impl crate::client::pool::PoolableStream for DuplexStream {
    fn can_share(&self) -> bool {
        false
    }
}

impl AsyncRead for DuplexStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.project().inner.poll_read(cx, buf)
    }
}

impl AsyncWrite for DuplexStream {
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

/// Client for connecting to a duplex server
///
/// This is a wrapper around a tokio channel, which is used to send
/// and recieve data between multiple clients and a server.
#[derive(Debug, Clone)]
pub struct DuplexClient {
    sender: tokio::sync::mpsc::Sender<DuplexConnectionRequest>,
}

impl DuplexClient {
    /// Connect to the other half of this duplex setup.
    ///
    /// The `max_buf_size` is the maximum size of the buffer used for the stream.
    pub async fn connect(&self, max_buf_size: usize) -> Result<DuplexStream, io::Error> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let request = DuplexConnectionRequest::new(tx, max_buf_size);
        self.sender
            .send(request)
            .await
            .map_err(|_| io::ErrorKind::ConnectionReset)?;
        Ok(rx.await.map_err(|_| io::ErrorKind::ConnectionReset)?)
    }
}

/// Gets sent to server to create connection
struct DuplexConnectionRequest {
    ack: tokio::sync::oneshot::Sender<DuplexStream>,
    max_buf_size: usize,
}

impl DuplexConnectionRequest {
    fn new(ack: tokio::sync::oneshot::Sender<DuplexStream>, max_buf_size: usize) -> Self {
        Self { ack, max_buf_size }
    }

    /// Tell waiting clients that the connection has been established
    fn ack(self, max_buf_size: Option<usize>) -> Result<DuplexStream, io::Error> {
        let max_buf_size = match max_buf_size {
            Some(size) => std::cmp::min(size, self.max_buf_size),
            None => self.max_buf_size,
        };

        let (tx, rx) = DuplexStream::new(max_buf_size);
        self.ack
            .send(tx)
            .map_err(|_| io::ErrorKind::ConnectionReset)?;
        Ok(rx)
    }
}

/// Stream of incoming connections, which implements the `Accept` trait.
///
/// This can be treated as a stream:
/// ```
/// # use hyperdriver::stream::duplex::{self, DuplexClient};
/// use futures_util::TryStreamExt;
/// # async fn demo_duplex() {
/// let (client, mut incoming) = duplex::pair();
///
/// let (client_conn, server_conn) = tokio::try_join!(client.connect(1024), incoming.try_next()).unwrap();
/// assert!(server_conn.is_some());
/// # }
/// ```
#[derive(Debug)]
pub struct DuplexIncoming {
    receiver: tokio::sync::mpsc::Receiver<DuplexConnectionRequest>,
    max_buf_size: Option<usize>,
}

impl DuplexIncoming {
    fn new(receiver: tokio::sync::mpsc::Receiver<DuplexConnectionRequest>) -> Self {
        Self {
            receiver,
            max_buf_size: None,
        }
    }

    /// Set the maximum buffer size for incoming connections
    pub fn with_max_buf_size(mut self, max_buf_size: usize) -> Self {
        self.max_buf_size = Some(max_buf_size);
        self
    }
}

#[cfg(feature = "server")]
impl Accept for DuplexIncoming {
    type Conn = DuplexStream;
    type Error = io::Error;

    fn poll_accept(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Conn, Self::Error>> {
        if let Some(request) = ready!(self.receiver.poll_recv(cx)) {
            let stream = request.ack(self.max_buf_size)?;
            Poll::Ready(Ok(stream))
        } else {
            Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()))
        }
    }
}

impl futures_core::Stream for DuplexIncoming {
    type Item = Result<DuplexStream, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(request) = ready!(self.receiver.poll_recv(cx)) {
            let stream = request.ack(self.max_buf_size)?;
            Poll::Ready(Some(Ok(stream)))
        } else {
            Poll::Ready(None)
        }
    }
}

/// Create a new duplex client and incoming pair.
///
/// The client can be cloned and re-used cheaply, and the incoming provides
/// a stream of incoming duplex connections.
pub fn pair() -> (DuplexClient, DuplexIncoming) {
    let (sender, receiver) = tokio::sync::mpsc::channel(32);
    (DuplexClient { sender }, DuplexIncoming::new(receiver))
}

#[cfg(test)]
mod test {
    #[test]
    fn test_duplex_addr_new_creates_unique_addresses() {
        use super::*;

        let addr1 = DuplexAddr::new();
        let addr2 = DuplexAddr::new();

        assert_ne!(
            addr1, addr2,
            "New DuplexAddr instances should have unique identities"
        );

        // Check that the Display implementation works correctly
        let addr1_str = format!("{}", addr1);
        let addr2_str = format!("{}", addr2);

        assert!(
            addr1_str.starts_with("duplex://"),
            "Display format should include protocol"
        );
        assert!(
            addr2_str.starts_with("duplex://"),
            "Display format should include protocol"
        );
        assert_ne!(
            addr1_str, addr2_str,
            "String representation should be different for different addresses"
        );
    }

    #[test]
    fn test_duplex_addr_clone_and_eq() {
        use super::*;

        let addr1 = DuplexAddr::new();
        let addr1_clone = addr1.clone();

        assert_eq!(
            addr1, addr1_clone,
            "Cloned address should equal the original"
        );
        assert_eq!(
            format!("{}", addr1),
            format!("{}", addr1_clone),
            "Cloned address should have same string representation"
        );
    }

    #[test]
    fn test_duplex_addr_default() {
        use super::*;

        let default_addr = DuplexAddr::new();
        let new_addr = DuplexAddr::new();

        // Default should be equivalent to calling new()
        assert_ne!(
            default_addr, new_addr,
            "Default should create a unique address"
        );

        // Debug formatting should work
        assert!(
            format!("{:?}", default_addr).contains("DuplexAddr"),
            "Debug output should contain struct name"
        );
    }

    #[tokio::test]
    async fn test_duplex() {
        use super::*;
        use futures_util::StreamExt;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (client, incoming) = pair();
        let mut incoming = incoming.fuse();

        let (mut client_stream, mut server_stream) =
            tokio::try_join!(client.connect(1024), async {
                incoming.next().await.unwrap()
            })
            .unwrap();

        let mut buf = [0u8; 1024];

        tokio::try_join!(
            client_stream.write_all(b"hello"),
            server_stream.read_exact(&mut buf[..5])
        )
        .unwrap();

        assert_eq!(&buf[..5], b"hello");

        tokio::try_join!(
            server_stream.write_all(b"world"),
            client_stream.read_exact(&mut buf[..5])
        )
        .unwrap();

        assert_eq!(&buf[..5], b"world");
    }
}
