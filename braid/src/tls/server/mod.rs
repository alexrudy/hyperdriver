//! Implementation of a server side TLS stream on top of tokio_rustls
//! which additionally provides connection information after the
//! handshake has been completed.

use core::panic;
use std::task::{Context, Poll};
use std::{fmt, io};
use std::{future::Future, pin::Pin};

use futures_core::ready;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_rustls::Accept;

use crate::info::{Connection, ConnectionInfo, TLSConnectionInfo};

pub mod acceptor;
pub mod connector;
pub(crate) mod info;
#[cfg(feature = "sni")]
pub mod sni;

pub use self::acceptor::TlsAcceptor;
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

#[derive(Debug)]
enum ConnectionInfoState {
    Pending(tokio::sync::oneshot::Sender<TLSConnectionInfo>),
    Received,
}

impl ConnectionInfoState {
    fn new(tx: tokio::sync::oneshot::Sender<TLSConnectionInfo>) -> Self {
        Self::Pending(tx)
    }

    fn send(&mut self, info: TLSConnectionInfo) {
        if matches!(self, ConnectionInfoState::Received) {
            panic!("ConnectionInfo already sent");
        }

        let sender = std::mem::replace(self, ConnectionInfoState::Received);

        match sender {
            ConnectionInfoState::Pending(tx) => {
                let _ = tx.send(info);
            }
            ConnectionInfoState::Received => unreachable!("ConnectionInfo already sent"),
        }
    }
}

#[derive(Debug)]
pub struct TlsStream<IO> {
    state: TlsState<IO>,
    info: ConnectionInfoState,
    pub(crate) rx: self::info::TlsConnectionInfoReciever,
}

impl<IO> TlsStream<IO>
where
    IO: AsyncRead + AsyncWrite + Unpin,
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
                    let sni = server_info.server_name().map(|s| s.to_string());
                    let info = TLSConnectionInfo::new(sni);
                    self.info.send(info.clone());

                    if let Some(local) = self.rx.local_addr() {
                        if let Some(remote) = self.rx.remote_addr() {
                            let host = info.sni.as_deref().unwrap_or("-");
                            tracing::trace!(%local, %remote, %host,  "TLS Handshake complete");
                        }
                    }
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
    IO: Connection,
{
    pub(crate) fn new(accept: Accept<IO>) -> Self {
        let (tx, rx) = tokio::sync::oneshot::channel();

        // We don't expect these to panic because we assume that the handshake has not finished when
        // this implementation is called. As long as no-one manually polls the future and then
        // does this after the future has returned ready, we should be okay.
        let info: ConnectionInfo = accept
            .get_ref()
            .map(|stream| stream.info())
            .expect("TLS handshake should have access to underlying IO");

        Self {
            state: TlsState::Handshake(accept),
            info: ConnectionInfoState::new(tx),
            rx: self::info::TlsConnectionInfoReciever::new(rx, info),
        }
    }
}

impl<IO> AsyncRead for TlsStream<IO>
where
    IO: AsyncRead + AsyncWrite + Unpin,
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
    IO: AsyncRead + AsyncWrite + Unpin,
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
