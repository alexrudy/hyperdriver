//! Implementation of a server side TLS stream on top of tokio_rustls
//! which additionally provides connection information after the
//! handshake has been completed.

use core::panic;
use std::task::{Context, Poll};
use std::{fmt, io};
use std::{future::Future, pin::Pin};

use futures_core::ready;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::Accept;

use crate::info::{ConnectionInfo, SocketAddr, TLSConnectionInfo};

pub mod acceptor;
pub mod connector;
pub(crate) mod info;
#[cfg(feature = "sni")]
pub mod sni;

pub use self::acceptor::TlsAcceptor;
pub use self::connector::TlsConnectLayer;

/// State tracks the process of accepting a connection and turning it into a stream.
enum TlsState {
    Handshake(tokio_rustls::Accept<TcpStream>),
    Streaming(tokio_rustls::server::TlsStream<TcpStream>),
}

impl fmt::Debug for TlsState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlsState::Handshake(_) => f.write_str("State::Handshake"),
            TlsState::Streaming(_) => f.write_str("State::Streaming"),
        }
    }
}

#[derive(Debug)]
enum ConnectionInfoState {
    Pending(tokio::sync::oneshot::Sender<ConnectionInfo>),
    Received(ConnectionInfo),
}

impl ConnectionInfoState {
    fn new(tx: tokio::sync::oneshot::Sender<ConnectionInfo>) -> Self {
        Self::Pending(tx)
    }

    fn send(&mut self, info: ConnectionInfo) {
        if matches!(self, ConnectionInfoState::Received(_)) {
            panic!("ConnectionInfo already sent");
        }

        let sender = std::mem::replace(self, ConnectionInfoState::Received(info.clone()));

        match sender {
            ConnectionInfoState::Pending(tx) => {
                let _ = tx.send(info);
            }
            ConnectionInfoState::Received(_) => unreachable!("ConnectionInfo already sent"),
        }
    }

    #[allow(dead_code)]
    fn get(&self) -> Option<&ConnectionInfo> {
        match self {
            ConnectionInfoState::Pending(_) => None,
            ConnectionInfoState::Received(info) => Some(info),
        }
    }
}

#[derive(Debug)]
pub struct TlsStream {
    state: TlsState,
    info: ConnectionInfoState,
    pub(crate) rx: self::info::TlsConnectionInfoReciever,
}

impl TlsStream {
    pub fn local_addr(&self) -> &SocketAddr {
        self.rx.local_addr()
    }

    pub fn remote_addr(&self) -> &SocketAddr {
        self.rx.remote_addr()
    }

    fn handshake<F, R>(&mut self, cx: &mut Context, action: F) -> Poll<io::Result<R>>
    where
        F: FnOnce(
            &mut tokio_rustls::server::TlsStream<TcpStream>,
            &mut Context,
        ) -> Poll<io::Result<R>>,
    {
        match self.state {
            TlsState::Handshake(ref mut accept) => match ready!(Pin::new(accept).poll(cx)) {
                Ok(mut stream) => {
                    // Take some action here when the handshake happens

                    let info = crate::info::ConnectionInfo::Tcp(TLSConnectionInfo::from_stream(
                        &stream,
                        self.rx.remote_addr().clone(),
                    ));
                    self.info.send(info.clone());

                    match &info {
                        ConnectionInfo::Tcp(tcp) => {
                            tracing::trace!(remote=?tcp.remote_addr, host=%tcp.tls.as_ref().and_then(|tls| tls.sni.as_deref()).unwrap_or("-"), "TLS Handshake complete");
                        }
                        _ => unreachable!("TLS Handshake on non-TCP connection"),
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

impl TlsStream {
    fn new(accept: Accept<TcpStream>, remote_addr: SocketAddr) -> Self {
        let (tx, rx) = tokio::sync::oneshot::channel();

        // We don't expect these to panic because we assume that the handshake has not finished when
        // this implementation is called. As long as no-one manually polls the future and then
        // does this after the future has returned ready, we should be okay.
        let local_addr = accept
            .get_ref()
            .and_then(|stream| stream.local_addr().ok())
            .expect("TLS handshake is not yet completed")
            .into();

        Self {
            state: TlsState::Handshake(accept),
            info: ConnectionInfoState::new(tx),
            rx: self::info::TlsConnectionInfoReciever::new(rx, remote_addr, local_addr),
        }
    }
}

impl AsyncRead for TlsStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<io::Result<()>> {
        let pin = self.get_mut();
        pin.handshake(cx, |stream, cx| Pin::new(stream).poll_read(cx, buf))
    }
}

impl AsyncWrite for TlsStream {
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
