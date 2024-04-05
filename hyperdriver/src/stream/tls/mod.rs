//! Support for braided streams which include Transport Layer security
//! and so involve a negotiation component.

pub mod client;
pub(crate) mod info;
pub mod server;

use std::{
    io,
    task::{Context, Poll},
};

pub use info::TlsConnectionInfo;
use tokio::io::{AsyncRead, AsyncWrite};

/// A stream that supports a TLS handshake.
pub trait TlsHandshakeStream: AsyncRead + AsyncWrite {
    /// Poll the handshake to completion.
    fn poll_handshake(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>>;
}
