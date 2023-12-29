//! Client-side TLS connector and stream which can take action once the
//! handshake is complete to provide information about the connection.

use core::task::{Context, Poll};
use std::sync::Arc;
use std::{fmt, io};
use std::{future::Future, pin::Pin};

use futures_core::future::BoxFuture;
use futures_core::ready;
use hyper::Uri;
use hyper_util::client::legacy::connect::HttpConnector;
use rustls::client::InvalidDnsNameError;
use rustls::ClientConfig;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpStream, ToSocketAddrs};
use tower::Service;

#[derive(Clone, Debug)]
pub struct TlsConnector {
    http: HttpConnector,
    tls: Arc<ClientConfig>,
}

impl TlsConnector {
    pub fn new(tls: Arc<ClientConfig>) -> Self {
        Self {
            http: HttpConnector::new(),
            tls,
        }
    }

    pub fn with_http(self, mut http: HttpConnector) -> Self {
        http.enforce_http(false);
        Self { http, ..self }
    }
}

impl Service<Uri> for TlsConnector {
    type Response = TlsStream<TcpStream>;

    type Error = Box<dyn std::error::Error + Send + Sync>;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.http.poll_ready(cx).map_err(|err| err.into())
    }

    fn call(&mut self, req: Uri) -> Self::Future {
        let domain = req
            .host()
            .ok_or(InvalidDnsNameError)
            .and_then(rustls::ServerName::try_from);
        let tls = self.tls.clone();
        let conn = self.http.call(req);

        let fut = async move {
            let stream = conn.await?;
            let connect =
                tokio_rustls::TlsConnector::from(tls).connect(domain?, stream.into_inner());
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(TlsStream::from(connect))
        };
        Box::pin(fut)
    }
}

enum State<IO> {
    Handshake(tokio_rustls::Connect<IO>),
    Streaming(tokio_rustls::client::TlsStream<IO>),
}

impl<IO> fmt::Debug for State<IO> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            State::Handshake(_) => f.write_str("State::Handshake"),
            State::Streaming(_) => write!(f, "State::Streaming"),
        }
    }
}

#[derive(Debug)]
pub struct TlsStream<IO> {
    state: State<IO>,
}

impl<IO> TlsStream<IO>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    pub fn new(
        stream: IO,
        domain: rustls::ServerName,
        roots: impl Into<Arc<rustls::RootCertStore>>,
    ) -> Self {
        let config = Arc::new(
            ClientConfig::builder()
                .with_safe_defaults()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        let connect = tokio_rustls::TlsConnector::from(config).connect(domain, stream);
        Self::from(connect)
    }
}

impl TlsStream<TcpStream> {
    pub async fn connect(
        addr: impl ToSocketAddrs,
        domain: rustls::ServerName,
        roots: impl Into<Arc<rustls::RootCertStore>>,
    ) -> io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Ok(Self::new(stream, domain, roots))
    }
}

impl<IO> TlsStream<IO>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    fn handshake<F, R>(&mut self, cx: &mut Context, action: F) -> Poll<io::Result<R>>
    where
        F: FnOnce(&mut tokio_rustls::client::TlsStream<IO>, &mut Context) -> Poll<io::Result<R>>,
    {
        match self.state {
            State::Handshake(ref mut accept) => match ready!(Pin::new(accept).poll(cx)) {
                Ok(mut stream) => {
                    // Take some action here when the handshake happens
                    //TODO: Provide client connection info?

                    // Back to processing the stream
                    let result = action(&mut stream, cx);
                    self.state = State::Streaming(stream);
                    result
                }
                Err(err) => Poll::Ready(Err(err)),
            },
            State::Streaming(ref mut stream) => action(stream, cx),
        }
    }
}

impl<IO> From<tokio_rustls::Connect<IO>> for TlsStream<IO> {
    fn from(accept: tokio_rustls::Connect<IO>) -> Self {
        Self {
            state: State::Handshake(accept),
        }
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin> AsyncRead for TlsStream<IO> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<io::Result<()>> {
        let pin = self.get_mut();
        pin.handshake(cx, |stream, cx| Pin::new(stream).poll_read(cx, buf))
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin> AsyncWrite for TlsStream<IO> {
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
            State::Handshake(_) => Poll::Ready(Ok(())),
            State::Streaming(ref mut stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.state {
            State::Handshake(_) => Poll::Ready(Ok(())),
            State::Streaming(ref mut stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}
