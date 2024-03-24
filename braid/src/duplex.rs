//! A duplex stream which also supports connection info.
//!
//! This is a wrapper around tokio's `DuplexStream`, while providing
//! a connection info struct which can be used to identify the connection.

use std::{
    io,
    pin::Pin,
    task::{ready, Context, Poll},
};

use http::uri::Authority;
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::info::{Connection, ConnectionInfo, Protocol};
use crate::server::Accept;

/// A duplex stream transports data entirely in memory within the tokio runtime.
#[derive(Debug)]
#[pin_project]
pub struct DuplexStream {
    #[pin]
    inner: tokio::io::DuplexStream,
    info: ConnectionInfo,
}

impl Connection for DuplexStream {
    fn info(&self) -> ConnectionInfo {
        self.info.clone()
    }
}

impl DuplexStream {
    /// Create a new duplex stream pair.
    ///
    /// The stream will be created with a buffer, and the `name` and `protocol` will be used to
    /// create the connection info. Normally, this method is not needed, an you should prefer
    /// using [`DuplexClient`] and [`DuplexIncoming`] together
    /// to create a client/server pair of duplex streams.
    pub fn new(name: Authority, protocol: Option<Protocol>, max_buf_size: usize) -> (Self, Self) {
        let (a, b) = tokio::io::duplex(max_buf_size);
        let info = ConnectionInfo::duplex(name, protocol);
        (
            DuplexStream {
                inner: a,
                info: info.clone(),
            },
            DuplexStream { inner: b, info },
        )
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
    name: Authority,
    sender: tokio::sync::mpsc::Sender<DuplexConnectionRequest>,
}

impl DuplexClient {
    /// Connect to the other half of this duplex stream.
    ///
    /// The `max_buf_size` is the maximum size of the buffer used for the stream.
    pub async fn connect(
        &self,
        max_buf_size: usize,
        protocol: Option<Protocol>,
    ) -> Result<DuplexStream, io::Error> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let request = DuplexConnectionRequest::new(self.name.clone(), tx, max_buf_size, protocol);
        self.sender
            .send(request)
            .await
            .map_err(|_| io::ErrorKind::ConnectionReset)?;
        Ok(rx.await.map_err(|_| io::ErrorKind::ConnectionReset)?)
    }
}

/// Gets sent to server to create connection
struct DuplexConnectionRequest {
    name: Authority,
    ack: tokio::sync::oneshot::Sender<DuplexStream>,
    max_buf_size: usize,
    protocol: Option<Protocol>,
}

impl DuplexConnectionRequest {
    fn new(
        name: Authority,
        ack: tokio::sync::oneshot::Sender<DuplexStream>,
        max_buf_size: usize,
        protocol: Option<Protocol>,
    ) -> Self {
        Self {
            name,
            ack,
            max_buf_size,
            protocol,
        }
    }

    /// Tell waiting clients that the connection has been established
    fn ack(self) -> Result<DuplexStream, io::Error> {
        let (tx, rx) = DuplexStream::new(self.name, self.protocol, self.max_buf_size);
        self.ack
            .send(tx)
            .map_err(|_| io::ErrorKind::ConnectionReset)?;
        Ok(rx)
    }
}

/// Stream of incoming connections
#[derive(Debug)]
pub struct DuplexIncoming {
    receiver: tokio::sync::mpsc::Receiver<DuplexConnectionRequest>,
}

impl DuplexIncoming {
    fn new(receiver: tokio::sync::mpsc::Receiver<DuplexConnectionRequest>) -> Self {
        Self { receiver }
    }
}

impl Accept for DuplexIncoming {
    type Conn = DuplexStream;
    type Error = io::Error;

    fn poll_accept(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Conn, Self::Error>> {
        if let Some(request) = ready!(self.receiver.poll_recv(cx)) {
            let stream = request.ack()?;
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
            let stream = request.ack()?;
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
pub fn pair(name: Authority) -> (DuplexClient, DuplexIncoming) {
    let (sender, receiver) = tokio::sync::mpsc::channel(32);
    (DuplexClient { name, sender }, DuplexIncoming::new(receiver))
}

#[cfg(test)]
mod test {
    use http::Version;

    #[tokio::test]
    async fn test_duplex() {
        use super::*;
        use futures_util::StreamExt;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let name: Authority = "test".parse().unwrap();

        let (client, incoming) = pair(name.clone());
        let mut incoming = incoming.fuse();

        let (mut client_stream, mut server_stream) = tokio::try_join!(
            client.connect(1024, Some(Protocol::Http(Version::HTTP_11))),
            async { incoming.next().await.unwrap() }
        )
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

        assert_eq!(client_stream.info().authority, Some(name));

        assert_eq!(&buf[..5], b"world");
    }
}
