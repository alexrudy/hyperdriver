//! Implementation of a server side TLS stream on top of tokio_rustls
//! which additionally provides connection information after the
//! handshake has been completed.

use core::panic;
use std::task::{Context, Poll};
use std::{fmt, io};
use std::{future::Future, pin::Pin};

use futures::ready;
use hyper::server::conn::AddrStream;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_rustls::Accept;

use crate::info::{ConnectionInfo, SocketAddr};

pub mod accpetor;
pub mod connector;
pub(crate) mod info;
#[cfg(feature = "sni")]
pub mod sni;

/// State tracks the process of accepting a connection and turning it into a stream.
enum TlsState {
    Handshake(tokio_rustls::Accept<AddrStream>),
    Streaming(tokio_rustls::server::TlsStream<AddrStream>),
}

impl fmt::Debug for TlsState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlsState::Handshake(accept) => {
                let remote = accept.get_ref().map(|stream| stream.remote_addr());
                if let Some(remote) = remote {
                    write!(f, "State::Handshake({remote:?})")
                } else {
                    f.write_str("State::Handshake")
                }
            }
            TlsState::Streaming(stream) => {
                let remote = stream.get_ref().0.remote_addr();
                write!(f, "State::Streaming({remote:?})")
            }
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
            &mut tokio_rustls::server::TlsStream<AddrStream>,
            &mut Context,
        ) -> Poll<io::Result<R>>,
    {
        match self.state {
            TlsState::Handshake(ref mut accept) => match ready!(Pin::new(accept).poll(cx)) {
                Ok(mut stream) => {
                    // Take some action here when the handshake happens

                    let info = crate::info::ConnectionInfo::from(&stream);
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

impl From<Accept<AddrStream>> for TlsStream {
    fn from(accept: Accept<AddrStream>) -> Self {
        let (tx, rx) = tokio::sync::oneshot::channel();

        // We don't expect these to panic because we assume that the handshake has not finished when
        // this implementation is called. As long as no-one manually polls the future and then
        // does this after the future has returned ready, we should be okay.
        let local_addr = accept
            .get_ref()
            .map(|stream| stream.local_addr().into())
            .expect("TLS handshake is not yet completed");
        let remote_addr = accept
            .get_ref()
            .map(|stream| stream.remote_addr().into())
            .expect("TLS handshake is not yet completed");

        Self {
            state: TlsState::Handshake(accept),
            info: ConnectionInfoState::new(tx),
            rx: self::info::TlsConnectionInfoReciever::new(rx, local_addr, remote_addr),
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
