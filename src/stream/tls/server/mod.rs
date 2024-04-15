//! Implementation of a server side TLS stream on top of tokio_rustls
//! which additionally provides connection information after the
//! handshake has been completed.

use std::task::{Context, Poll};
use std::{fmt, io};
use std::{future::Future, pin::Pin};

use futures_core::ready;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_rustls::Accept;

use super::info::{TlsConnectionInfo, TlsConnectionInfoReciever, TlsConnectionInfoSender};
use super::TlsHandshakeStream;
use crate::stream::info::{ConnectionInfo, HasConnectionInfo};

#[cfg(all(feature = "server", feature = "stream"))]
pub mod acceptor;

#[cfg(all(feature = "server", feature = "stream"))]
pub mod connector;
#[cfg(feature = "sni")]
pub mod sni;

#[cfg(all(feature = "server", feature = "stream"))]
pub use self::acceptor::TlsAcceptor;

#[cfg(all(feature = "server", feature = "stream"))]
pub use self::connector::TlsConnectLayer;

/// State tracks the process of accepting a connection and turning it into a stream.
enum TlsState<IO> {
    Handshake(tokio_rustls::Accept<IO>),
    Streaming(tokio_rustls::server::TlsStream<IO>),
}

impl<IO> fmt::Debug for TlsState<IO> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlsState::Handshake(_) => f.write_str("State::Handshake"),
            TlsState::Streaming(_) => f.write_str("State::Streaming"),
        }
    }
}

/// A TLS stream, generic over the underlying IO.
#[derive(Debug)]
pub struct TlsStream<IO>
where
    IO: HasConnectionInfo,
{
    state: TlsState<IO>,
    tx: TlsConnectionInfoSender<IO::Addr>,
    pub(crate) rx: TlsConnectionInfoReciever<IO::Addr>,
}

impl<IO> TlsHandshakeStream for TlsStream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    IO::Addr: Unpin,
{
    fn poll_handshake(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.handshake(cx, |_, _| Poll::Ready(Ok(())))
    }
}

impl<IO> TlsStream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
{
    fn handshake<F, R>(&mut self, cx: &mut Context, action: F) -> Poll<io::Result<R>>
    where
        F: FnOnce(&mut tokio_rustls::server::TlsStream<IO>, &mut Context) -> Poll<io::Result<R>>,
    {
        match self.state {
            TlsState::Handshake(ref mut accept) => match ready!(Pin::new(accept).poll(cx)) {
                Ok(mut stream) => {
                    // Take some action here when the handshake happens

                    let (_, server_info) = stream.get_ref();
                    let info = TlsConnectionInfo::server(server_info);
                    self.tx.send(info.clone());

                    let host = info.server_name.as_deref().unwrap_or("-");
                    tracing::trace!(local=%self.rx.local_addr(), remote=%self.rx.remote_addr(), %host,  "TLS Handshake complete");

                    // Back to processing the stream
                    let result = action(&mut stream, cx);
                    self.state = TlsState::Streaming(stream);
                    result
                }
                Err(err) => Poll::Ready(Err(err)),
            },
            TlsState::Streaming(ref mut stream) => action(stream, cx),
        }
    }
}

impl<IO> TlsStream<IO>
where
    IO: HasConnectionInfo,
    IO::Addr: Clone,
{
    pub(crate) fn new(accept: Accept<IO>) -> Self {
        // We don't expect these to panic because we assume that the handshake has not finished when
        // this implementation is called. As long as no-one manually polls the future and then
        // does this after the future has returned ready, we should be okay.
        let info: ConnectionInfo<IO::Addr> = accept
            .get_ref()
            .map(|stream| stream.info())
            .expect("TLS handshake should have access to underlying IO");

        let (tx, rx) = TlsConnectionInfo::channel(info);

        Self {
            state: TlsState::Handshake(accept),
            tx,
            rx,
        }
    }
}

impl<IO> HasConnectionInfo for TlsStream<IO>
where
    IO: HasConnectionInfo,
    IO::Addr: Clone,
{
    type Addr = IO::Addr;

    fn info(&self) -> ConnectionInfo<Self::Addr> {
        self.rx.info()
    }
}

impl<IO> AsyncRead for TlsStream<IO>
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

impl<IO> AsyncWrite for TlsStream<IO>
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
            TlsState::Handshake(_) => Poll::Ready(Ok(())),
            TlsState::Streaming(ref mut stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.state {
            TlsState::Handshake(_) => Poll::Ready(Ok(())),
            TlsState::Streaming(ref mut stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}