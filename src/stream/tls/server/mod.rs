//! Implementation of a server side TLS stream on top of tokio_rustls
//! which additionally provides connection information after the
//! handshake has been completed.

use std::task::{Context, Poll};
use std::{fmt, io};
use std::{future::Future, pin::Pin};

use futures_core::ready;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_rustls::Accept;

use super::{TlsHandshakeInfo, TlsHandshakeStream};
use crate::info::tls::{channel, TlsConnectionInfoReciever, TlsConnectionInfoSender};
use crate::info::{ConnectionInfo, HasConnectionInfo, TlsConnectionInfo};

pub mod acceptor;

pub mod connector;
#[cfg(feature = "sni")]
pub mod sni;

pub use self::acceptor::TlsAcceptor;
pub use self::connector::TlsConnectionInfoLayer;

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
    tx: TlsConnectionInfoSender,
    pub(crate) rx: TlsConnectionInfoReciever,
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

impl<IO> TlsHandshakeInfo for TlsStream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    IO::Addr: Unpin,
{
    fn recv(&self) -> TlsConnectionInfoReciever {
        self.rx.clone()
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

                    let (io, server_info) = stream.get_ref();
                    let info = TlsConnectionInfo::server(server_info);
                    self.tx.send(info.clone());

                    let host = info.server_name.as_deref().unwrap_or("-");
                    tracing::trace!(local=%io.info().local_addr(), remote=%io.info().remote_addr(), %host,  "TLS Handshake complete");

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
    /// Create a new `TlsStream` from an `Accept` future.
    ///
    /// This adapts the underlying `rustls` stream to provide connection information.
    pub fn new(accept: Accept<IO>) -> Self {
        let (tx, rx) = channel();

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
        match &self.state {
            TlsState::Handshake(a) => a
                .get_ref()
                .map(|io| io.info())
                .expect("handshake is complete"),
            TlsState::Streaming(s) => s.get_ref().0.info(),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::convert::Infallible;

    use http::Response;

    use tower::make::Shared;
    use tower::Service;

    use tracing::Instrument as _;

    use crate::client::conn::DuplexTransport;
    use crate::client::conn::Transport as _;
    use crate::client::conn::TransportTlsExt;

    use crate::stream::server::AcceptExt as _;

    #[tokio::test]
    async fn tls_client_server() {
        let _ = tracing_subscriber::fmt::try_init();
        let _ = color_eyre::install();

        let _guard = tracing::info_span!("tls").entered();

        let service = tower::service_fn(|_: http::Request<crate::Body>| async {
            Ok::<_, Infallible>(Response::new(crate::Body::empty()))
        });

        let (client, incoming) = crate::stream::duplex::pair("test".parse().unwrap());

        let acceptor = crate::stream::server::Acceptor::from(incoming)
            .with_tls(crate::fixtures::tls_server_config().into());

        let mut client = DuplexTransport::new(1024, None, client)
            .with_tls(crate::fixtures::tls_client_config().into());

        let client = async move {
            let conn = client
                .connect("https://example.com".parse().unwrap())
                .await
                .unwrap();

            tracing::debug!("client connected");

            let (mut stream, _) = conn.into_parts();
            stream.finish_handshake().await.unwrap();

            tracing::debug!("client handshake finished");

            stream
        }
        .instrument(tracing::info_span!("client"));

        let server = async move {
            let mut conn = acceptor.accept().await.unwrap();

            tracing::debug!("server accepted");

            let mut make_service = Shared::new(service);

            let mut svc = Service::call(&mut make_service, &conn).await.unwrap();
            tracing::debug!("server connected");

            conn.finish_handshake().await.unwrap();
            tracing::debug!("server handshake finished");

            let _ = tower::Service::call(&mut svc, http::Request::new(crate::Body::empty()))
                .await
                .unwrap();

            tracing::debug!("server request handled");
            conn
        }
        .instrument(tracing::info_span!("server"));

        let (stream, conn) = tokio::join!(client, server);
        drop((stream, conn));
    }
}
