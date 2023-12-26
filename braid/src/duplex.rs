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
use hyper::server::accept::Accept;
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::info::{DuplexConnectionInfo, Protocol};

#[derive(Debug)]
#[pin_project]
pub struct DuplexStream {
    #[pin]
    inner: tokio::io::DuplexStream,
    info: DuplexConnectionInfo,
}

impl DuplexStream {
    pub fn info(&self) -> &DuplexConnectionInfo {
        &self.info
    }

    pub fn new(name: Authority, protocol: Protocol, max_buf_size: usize) -> (Self, Self) {
        let (a, b) = tokio::io::duplex(max_buf_size);
        let info = DuplexConnectionInfo::new(name, protocol);
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
    pub fn new(name: Authority) -> (DuplexClient, DuplexIncoming) {
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        (DuplexClient { name, sender }, DuplexIncoming::new(receiver))
    }

    pub async fn connect(
        &self,
        max_buf_size: usize,
        protocol: Protocol,
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
    protocol: Protocol,
}

impl DuplexConnectionRequest {
    fn new(
        name: Authority,
        ack: tokio::sync::oneshot::Sender<DuplexStream>,
        max_buf_size: usize,
        protocol: Protocol,
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
    ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
        if let Some(request) = ready!(self.receiver.poll_recv(cx)) {
            let stream = request.ack()?;
            Poll::Ready(Some(Ok(stream)))
        } else {
            Poll::Ready(None)
        }
    }
}

impl futures::Stream for DuplexIncoming {
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
