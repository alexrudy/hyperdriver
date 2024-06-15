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

use crate::info::tls::HasTlsConnectionInfo;
use crate::info::{ConnectionInfo, HasConnectionInfo};

use super::TlsHandshakeStream;
use crate::info::TlsConnectionInfo;

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
/// request to the underlying stream.
#[derive(Debug)]
pub struct ClientTlsStream<IO>
where
    IO: HasConnectionInfo,
{
    state: State<IO>,
    info: ConnectionInfo<IO::Addr>,
    tls: Option<TlsConnectionInfo>,
}

impl<IO> HasConnectionInfo for ClientTlsStream<IO>
where
    IO: HasConnectionInfo,
    IO::Addr: Clone,
{
    type Addr = IO::Addr;

    fn info(&self) -> ConnectionInfo<Self::Addr> {
        self.info.clone()
    }
}

impl<IO> HasTlsConnectionInfo for ClientTlsStream<IO>
where
    IO: HasConnectionInfo,
    IO::Addr: Clone,
{
    fn tls_info(&self) -> Option<&TlsConnectionInfo> {
        self.tls.as_ref()
    }
}

impl<IO> TlsHandshakeStream for ClientTlsStream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    IO::Addr: Unpin,
{
    fn poll_handshake(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.handshake(cx, |_, _| Poll::Ready(Ok(())))
    }
}

impl<IO> ClientTlsStream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
{
    /// Finish the TLS handshake.
    pub async fn finish_handshake(&mut self) -> io::Result<()> {
        futures_util::future::poll_fn(|cx| self.handshake(cx, |_, _| Poll::Ready(Ok(())))).await
    }
}

impl<IO> ClientTlsStream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    IO::Addr: Clone,
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

impl ClientTlsStream<TcpStream> {
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

impl<IO> ClientTlsStream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
{
    fn handshake<F, R>(&mut self, cx: &mut Context, action: F) -> Poll<io::Result<R>>
    where
        F: FnOnce(&mut tokio_rustls::client::TlsStream<IO>, &mut Context) -> Poll<io::Result<R>>,
    {
        match self.state {
            State::Handshake(ref mut connect) => match ready!(Pin::new(connect).poll(cx)) {
                Ok(mut stream) => {
                    // Take some action here when the handshake happens
                    let (_, client_info) = stream.get_ref();
                    let info = TlsConnectionInfo::client(client_info);
                    self.tls = Some(info);

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

impl<IO> From<tokio_rustls::Connect<IO>> for ClientTlsStream<IO>
where
    IO: HasConnectionInfo,
    IO::Addr: Clone,
{
    fn from(accept: tokio_rustls::Connect<IO>) -> Self {
        let stream = accept.get_ref().expect("tls connect should have stream");

        let info = stream.info();

        Self {
            state: State::Handshake(accept),
            info,
            tls: None,
        }
    }
}

impl<IO> AsyncRead for ClientTlsStream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    IO::Addr: Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<io::Result<()>> {
        let pin = self.get_mut();
        pin.handshake(cx, |stream, cx| Pin::new(stream).poll_read(cx, buf))
    }
}

impl<IO> AsyncWrite for ClientTlsStream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    IO::Addr: Unpin,
{
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
