//! Support for braided streams which include Transport Layer security
//! and so involve a negotiation component.

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "server")]
pub mod server;

use std::{
    io,
    task::{Context, Poll},
};

pub use crate::info::TlsConnectionInfo;
use futures_core::Future;
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

/// A stream that supports a TLS handshake.
pub trait TlsHandshakeStream: AsyncRead + AsyncWrite {
    /// Poll the handshake to completion.
    fn poll_handshake(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>>;
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
    NoTls: AsyncRead + AsyncWrite + Unpin,
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
