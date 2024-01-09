//! Client-side TLS connector and stream which can take action once the
//! handshake is complete to provide information about the connection.

use core::task::{Context, Poll};
use std::sync::Arc;
use std::{fmt, io};
use std::{future::Future, pin::Pin};

use futures_core::ready;
use rustls::ClientConfig;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpStream, ToSocketAddrs};

use crate::info::{Connection, ConnectionInfo};

use super::info::{TlsConnectionInfo, TlsConnectionInfoReciever, TlsConnectionInfoSender};

#[cfg(feature = "connector")]
mod connector;

#[cfg(feature = "connector")]
pub use connector::TlsConnector;

enum State<IO> {
    Handshake(tokio_rustls::Connect<IO>),
    Streaming(tokio_rustls::client::TlsStream<IO>),
}

impl<IO> fmt::Debug for State<IO> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            State::Handshake(_) => f.write_str("State::Handshake"),
            State::Streaming(_) => write!(f, "State::Streaming"),
        }
    }
}

/// A TLS stream, generic over the underlying IO.
///
/// This stream implements a delayed handshake by default, where
/// the handshake won't be completed until the first read/write
/// request to the underlying stream. The handshake can be forced
/// to completion using the [`finish_handshake`](TlsStream::finish_handshake) method.
#[derive(Debug)]
pub struct TlsStream<IO> {
    state: State<IO>,
    tx: TlsConnectionInfoSender,
    rx: TlsConnectionInfoReciever,
}

impl<IO> TlsStream<IO> {
    /// Get the connection info for this stream.
    pub async fn info(&self) -> io::Result<ConnectionInfo> {
        self.rx.recv().await
    }
}

impl<IO> TlsStream<IO>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    /// Finish the TLS handshake.
    pub async fn finish_handshake(&mut self) -> io::Result<()> {
        futures_util::future::poll_fn(|cx| self.handshake(cx, |_, _| Poll::Ready(Ok(())))).await
    }
}

impl<IO> TlsStream<IO>
where
    IO: Connection + AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new TLS stream from the given IO, with a domain name and TLS configuration.
    pub fn new(stream: IO, domain: &str, config: Arc<ClientConfig>) -> Self {
        let domain = rustls::pki_types::ServerName::try_from(domain)
            .expect("should be valid dns name")
            .to_owned();

        let connect = tokio_rustls::TlsConnector::from(config).connect(domain, stream);
        Self::from(connect)
    }
}

impl TlsStream<TcpStream> {
    /// Connect to the given tcp address, using the given domain name and TLS configuration.
    pub async fn connect(
        addr: impl ToSocketAddrs,
        domain: &str,
        config: Arc<ClientConfig>,
    ) -> io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Ok(Self::new(stream, domain, config))
    }
}

impl<IO> TlsStream<IO>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    fn handshake<F, R>(&mut self, cx: &mut Context, action: F) -> Poll<io::Result<R>>
    where
        F: FnOnce(&mut tokio_rustls::client::TlsStream<IO>, &mut Context) -> Poll<io::Result<R>>,
    {
        match self.state {
            State::Handshake(ref mut accept) => match ready!(Pin::new(accept).poll(cx)) {
                Ok(mut stream) => {
                    // Take some action here when the handshake happens
                    let (_, client_info) = stream.get_ref();
                    let info = TlsConnectionInfo::client(client_info);
                    self.tx.send(info.clone());

                    // Back to processing the stream
                    let result = action(&mut stream, cx);
                    self.state = State::Streaming(stream);
                    result
                }
                Err(err) => Poll::Ready(Err(err)),
            },
            State::Streaming(ref mut stream) => action(stream, cx),
        }
    }
}

impl<IO> From<tokio_rustls::Connect<IO>> for TlsStream<IO>
where
    IO: Connection,
{
    fn from(accept: tokio_rustls::Connect<IO>) -> Self {
        let stream = accept.get_ref().expect("tls connect should have stream");

        let info = stream.info();

        let (tx, rx) = TlsConnectionInfo::channel(info);

        Self {
            state: State::Handshake(accept),
            tx,
            rx,
        }
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin> AsyncRead for TlsStream<IO> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<io::Result<()>> {
        let pin = self.get_mut();
        pin.handshake(cx, |stream, cx| Pin::new(stream).poll_read(cx, buf))
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin> AsyncWrite for TlsStream<IO> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let pin = self.get_mut();
        pin.handshake(cx, |stream, cx| Pin::new(stream).poll_write(cx, buf))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.state {
            State::Handshake(_) => Poll::Ready(Ok(())),
            State::Streaming(ref mut stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.state {
            State::Handshake(_) => Poll::Ready(Ok(())),
            State::Streaming(ref mut stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}
