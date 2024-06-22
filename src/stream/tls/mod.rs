//! Support for braided streams which include Transport Layer security
//! and so involve a negotiation component.

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "server")]
pub mod server;

use std::{
    future::poll_fn,
    io,
    pin::Pin,
    task::{Context, Poll},
};

#[cfg(feature = "server")]
use crate::info::tls::TlsConnectionInfoReciever;
pub use crate::info::TlsConnectionInfo;
use futures_core::Future;
use futures_util::FutureExt;
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

/// A stream that supports a TLS handshake.
pub trait TlsHandshakeStream: AsyncRead + AsyncWrite + Send {
    /// Poll the handshake to completion.
    fn poll_handshake(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>>;

    /// Finish the TLS handshake.
    ///
    /// This method will drive the connection asynchronosly allowing you to wait
    /// for the TLS handshake to complete. If this method is not called, the TLS handshake
    /// will be completed the first time the connection is used.
    fn finish_handshake(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), io::Error>> + Send + '_>> {
        poll_fn(|cx| self.poll_handshake(cx)).boxed()
    }
}

/// A stream which can provide information about the TLS handshake.
#[cfg(feature = "server")]
pub(crate) trait TlsHandshakeInfo: TlsHandshakeStream {
    fn recv(&self) -> TlsConnectionInfoReciever;
}

/// Dispatching wrapper for optionally supporting TLS
#[derive(Debug)]
#[pin_project(project=BraidProjection)]
pub enum TlsBraid<Tls, NoTls> {
    /// A stream without TLS
    NoTls(#[pin] NoTls),

    /// A stream with TLS
    Tls(#[pin] Tls),
}

impl<Tls, NoTls> TlsHandshakeStream for TlsBraid<Tls, NoTls>
where
    Tls: TlsHandshakeStream + Unpin,
    NoTls: AsyncRead + AsyncWrite + Send + Unpin,
{
    fn poll_handshake(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        match self {
            TlsBraid::NoTls(_) => Poll::Ready(Ok(())),
            TlsBraid::Tls(ref mut stream) => stream.poll_handshake(cx),
        }
    }
}

#[cfg(feature = "server")]
impl<Tls, NoTls> TlsHandshakeInfo for TlsBraid<Tls, NoTls>
where
    Tls: TlsHandshakeInfo + Unpin,
    NoTls: AsyncRead + AsyncWrite + Send + Unpin,
{
    fn recv(&self) -> TlsConnectionInfoReciever {
        match self {
            TlsBraid::NoTls(_) => TlsConnectionInfoReciever::empty(),
            TlsBraid::Tls(stream) => stream.recv(),
        }
    }
}

macro_rules! dispatch {
    ($driver:ident.$method:ident($($args:expr),+)) => {

        match $driver.project() {
            BraidProjection::NoTls(stream) => stream.$method($($args),+),
            BraidProjection::Tls(stream) => stream.$method($($args),+),
        }
    };
}

impl<Tls, NoTls> AsyncRead for TlsBraid<Tls, NoTls>
where
    Tls: AsyncRead,
    NoTls: AsyncRead,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        dispatch!(self.poll_read(cx, buf))
    }
}

impl<Tls, NoTls> AsyncWrite for TlsBraid<Tls, NoTls>
where
    Tls: AsyncWrite,
    NoTls: AsyncWrite,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        dispatch!(self.poll_write(cx, buf))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        dispatch!(self.poll_flush(cx))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        dispatch!(self.poll_shutdown(cx))
    }
}

impl<Tls, NoTls> From<NoTls> for TlsBraid<Tls, NoTls> {
    fn from(stream: NoTls) -> Self {
        Self::NoTls(stream)
    }
}

/// Extension trait for `TlsHandshakeStream`.
pub trait TlsHandshakeExt: TlsHandshakeStream {
    /// Perform a TLS handshake.
    fn handshake(&mut self) -> TlsHandshakeFuture<'_, Self>
    where
        Self: Sized,
    {
        TlsHandshakeFuture::new(self)
    }
}

impl<T> TlsHandshakeExt for T where T: TlsHandshakeStream {}

/// A future that resolves once the TLS handshake is complete.
#[derive(Debug)]
#[pin_project]
pub struct TlsHandshakeFuture<'s, S> {
    stream: &'s mut S,
}

impl<'s, S> TlsHandshakeFuture<'s, S> {
    fn new(stream: &'s mut S) -> Self {
        Self { stream }
    }
}

impl<'s, S> Future for TlsHandshakeFuture<'s, S>
where
    S: TlsHandshakeStream,
{
    type Output = Result<(), io::Error>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().stream.poll_handshake(cx)
    }
}
