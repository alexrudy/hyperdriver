//! Hyper TLS Acceptor with some support for tracing.

use core::task::{Context, Poll};
use std::pin::Pin;
use std::{io, sync::Arc};

use futures::ready;
use hyper::server::accept::Accept;
use hyper::server::conn::AddrIncoming;
use pin_project::pin_project;
use rustls::ServerConfig;

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
    incoming: AddrIncoming,
}

pub(super) use super::TlsStream;

impl TlsAcceptor {
    /// Create a new TLS Acceptor with the given [rustls::ServerConfig] and [hyper::server::conn::AddrIncoming].
    pub fn new(config: Arc<ServerConfig>, incoming: AddrIncoming) -> TlsAcceptor {
        TlsAcceptor { config, incoming }
    }
}

impl Accept for TlsAcceptor {
    type Conn = TlsStream;
    type Error = io::Error;

    fn poll_accept(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
        let this = self.project();

        match ready!(this.incoming.poll_accept(cx)) {
            // A new TCP connection is ready to be accepted.
            Some(Ok(stream)) => {
                let accept =
                    tokio_rustls::TlsAcceptor::from(Arc::clone(this.config)).accept(stream);
                Poll::Ready(Some(Ok(accept.into())))
            }

            // An error occurred while accepting a new TCP connection.
            Some(Err(e)) => Poll::Ready(Some(Err(e))),

            // No new TCP connection is ready to be accepted.
            None => Poll::Ready(None),
        }
    }
}
