//! Hyper TLS Acceptor with some support for tracing.

use core::task::{Context, Poll};
use std::pin::Pin;
use std::sync::Arc;

use futures_core::ready;
use pin_project::pin_project;
use rustls::ServerConfig;

use crate::info::HasConnectionInfo;

use crate::stream::server::Accept;
/// TLS Acceptor which uses a [rustls::ServerConfig] to accept connections
/// and start a TLS handshake.
///
/// The actual handshake is handled in the [super::TlsStream] type.
///
/// The TLS acceptor implements the [Accept] trait from hyperdriver.
#[derive(Debug)]
#[pin_project]
pub struct TlsAcceptor<A> {
    config: Arc<ServerConfig>,
    #[pin]
    incoming: A,
}

pub(super) use super::TlsStream;

impl<A> TlsAcceptor<A> {
    /// Create a new TLS Acceptor with the given [rustls::ServerConfig] and [tokio::net::TcpListener].
    pub fn new(config: Arc<ServerConfig>, incoming: A) -> Self {
        TlsAcceptor { config, incoming }
    }
}

impl<A> Accept for TlsAcceptor<A>
where
    A: Accept,
    A::Conn: HasConnectionInfo,
    <A::Conn as HasConnectionInfo>::Addr: Clone + Unpin + Send + Sync + 'static,
{
    type Conn = TlsStream<A::Conn>;
    type Error = A::Error;

    fn poll_accept(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Conn, Self::Error>> {
        let this = self.project();

        match ready!(this.incoming.poll_accept(cx)) {
            // A new TCP connection is ready to be accepted.
            Ok(stream) => {
                let accept =
                    tokio_rustls::TlsAcceptor::from(Arc::clone(this.config)).accept(stream);
                Poll::Ready(Ok(TlsStream::new(accept)))
            }

            // An error occurred while accepting a new TCP connection.
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

/// Extension trait for the [Accept] trait to add a method to wrap the acceptor in a TLS acceptor.
pub trait TlsAcceptExt: Accept {
    /// Wrap the acceptor in a TLS acceptor using the given [rustls::ServerConfig].
    fn tls(self, config: Arc<ServerConfig>) -> TlsAcceptor<Self>
    where
        Self: Sized,
    {
        TlsAcceptor::new(config, self)
    }
}

impl<A> TlsAcceptExt for A where A: Accept {}
