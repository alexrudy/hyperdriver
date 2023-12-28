//! Hyper TLS Acceptor with some support for tracing.

use core::task::{Context, Poll};
use std::pin::Pin;
use std::{io, sync::Arc};

use futures_core::ready;
use pin_project::pin_project;
use rustls::ServerConfig;
use tokio::net::TcpListener;

/// TLS Acceptor which uses a [rustls::ServerConfig] to accept connections
/// and start a TLS handshake.
///
/// The actual handshake is handled in the [super::TlsStream] type.
///
/// The TLS acceptor implements the [Accept] trait from hyper.
#[derive(Debug)]
#[pin_project]
pub struct TlsAcceptor {
    config: Arc<ServerConfig>,
    #[pin]
    incoming: TcpListener,
}

use crate::server::Accept;

pub(super) use super::TlsStream;

impl TlsAcceptor {
    /// Create a new TLS Acceptor with the given [rustls::ServerConfig] and [tokio::net:TcpListener].
    pub fn new(config: Arc<ServerConfig>, incoming: TcpListener) -> TlsAcceptor {
        TlsAcceptor { config, incoming }
    }
}

impl Accept for TlsAcceptor {
    type Conn = TlsStream;
    type Error = io::Error;

    fn poll_accept(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Conn, Self::Error>> {
        let this = self.project();

        match ready!(this.incoming.poll_accept(cx)) {
            // A new TCP connection is ready to be accepted.
            Ok((stream, remote_addr)) => {
                let accept =
                    tokio_rustls::TlsAcceptor::from(Arc::clone(this.config)).accept(stream);
                Poll::Ready(Ok(TlsStream::new(accept, remote_addr.into())))
            }

            // An error occurred while accepting a new TCP connection.
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}
