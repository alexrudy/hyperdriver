//! Client side of the Braid stream
//!
//! The server and client are differentiated for TLS support, but otherwise,
//! TCP and Duplex streams are the same whether they are server or client.

#[cfg(feature = "stream")]
use std::net::SocketAddr;

#[cfg(feature = "tls")]
use std::sync::Arc;
use std::task::{Context, Poll};

#[cfg(feature = "tls")]
pub use self::tls::TlsStream;
use hyperdriver_core::info::HasConnectionInfo;
#[cfg(feature = "tls")]
use hyperdriver_core::info::HasTlsConnectionInfo;
#[cfg(feature = "stream")]
use hyperdriver_core::stream::duplex::DuplexStream;
#[cfg(feature = "tls")]
use hyperdriver_core::stream::tls::TlsHandshakeStream;
#[cfg(feature = "stream")]
use hyperdriver_core::stream::Braid;
#[cfg(feature = "tls")]
use hyperdriver_core::stream::TlsBraid;
#[cfg(feature = "stream")]
use hyperdriver_core::stream::{TcpStream, UnixStream};
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

#[cfg(feature = "mocks")]
pub mod mock;
#[cfg(feature = "tls")]
pub(crate) mod tls;

#[cfg(feature = "stream")]
/// A stream which can handle multiple different underlying transports, and TLS
/// through a unified type.
///
/// This is the client side of the Braid stream.
#[derive(Debug)]
#[pin_project]
pub struct Stream<IO = Braid>
where
    IO: HasConnectionInfo,
{
    #[cfg(feature = "tls")]
    #[pin]
    inner: TlsBraid<TlsStream<IO>, IO>,

    #[cfg(not(feature = "tls"))]
    #[pin]
    inner: IO,
}

#[cfg(not(feature = "stream"))]
/// A stream which can handle multiple different underlying transports, and TLS
/// through a unified type.
///
/// This is the client side of the Braid stream.
#[derive(Debug)]
#[pin_project]
pub struct Stream<IO>
where
    IO: HasConnectionInfo,
{
    #[cfg(feature = "tls")]
    #[pin]
    inner: TlsBraid<TlsStream<IO>, IO>,

    #[cfg(not(feature = "tls"))]
    #[pin]
    inner: IO,
}

#[cfg(feature = "stream")]
impl Stream {
    /// Connect to a server via TCP at the given address.
    ///
    /// For other connection methods/types, use the appropriate `From` impl.
    pub async fn connect(addr: impl Into<SocketAddr>) -> std::io::Result<Self> {
        let stream = TcpStream::connect(addr.into()).await?;
        Ok(stream.into())
    }
}

impl<IO> Stream<IO>
where
    IO: HasConnectionInfo,
{
    /// Create a new client stream from an existing connection.
    pub fn new(inner: IO) -> Self {
        Stream {
            #[cfg(feature = "tls")]
            inner: TlsBraid::NoTls(inner),

            #[cfg(not(feature = "tls"))]
            inner,
        }
    }

    /// Map the inner stream to a new type.
    pub fn map<F, T>(self, f: F) -> Stream<T>
    where
        F: FnOnce(IO) -> T,
        T: HasConnectionInfo,
    {
        Stream {
            #[cfg(feature = "tls")]
            inner: match self.inner {
                TlsBraid::NoTls(inner) => TlsBraid::NoTls(f(inner)),
                TlsBraid::Tls(_) => panic!("Stream::map called on a TLS stream"),
            },

            #[cfg(not(feature = "tls"))]
            inner: f(self.inner),
        }
    }
}

#[cfg(feature = "tls")]
impl<IO> Stream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    IO::Addr: Clone,
{
    /// Add TLS to the underlying stream.
    ///
    /// # Panics
    /// TLS can only be added once. If this is called twice, it will panic.
    ///
    /// # Arguments
    ///
    /// * `domain` - The domain name to connect to. This is used for SNI.
    /// * `config` - The TLS client configuration to use.
    pub fn tls(self, domain: &str, config: Arc<rustls::ClientConfig>) -> Self {
        let core = match self.inner {
            TlsBraid::NoTls(core) => core,
            TlsBraid::Tls(_) => panic!("Stream::tls called twice"),
        };

        Stream {
            inner: TlsBraid::Tls(TlsStream::new(core, domain, config)),
        }
    }
}

#[cfg(feature = "tls")]
impl<IO> TlsHandshakeStream for Stream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Send + Unpin + 'static,
    IO::Addr: Send + Unpin + Clone,
{
    #[inline]
    fn poll_handshake(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        self.inner.poll_handshake(cx)
    }
}

impl<IO> HasConnectionInfo for Stream<IO>
where
    IO: HasConnectionInfo,
    IO::Addr: Unpin + Clone,
{
    type Addr = IO::Addr;

    /// Get information about the connection.
    ///
    /// This method is async because TLS information isn't available until the handshake
    /// is complete. This method will not return until the handshake is complete.
    fn info(&self) -> hyperdriver_core::info::ConnectionInfo<IO::Addr> {
        #[cfg(feature = "tls")]
        match self.inner {
            TlsBraid::Tls(ref stream) => stream.info(),
            TlsBraid::NoTls(ref stream) => stream.info(),
        }

        #[cfg(not(feature = "tls"))]
        self.inner.info()
    }
}

#[cfg(feature = "tls")]
impl<IO> HasTlsConnectionInfo for Stream<IO>
where
    IO: HasConnectionInfo,
    IO::Addr: Unpin + Clone,
{
    fn tls_info(&self) -> Option<&hyperdriver_core::info::TlsConnectionInfo> {
        match self.inner {
            TlsBraid::Tls(ref stream) => stream.tls_info(),
            TlsBraid::NoTls(_) => None,
        }
    }
}

#[cfg(feature = "stream")]
impl From<TcpStream> for Stream {
    fn from(stream: TcpStream) -> Self {
        Stream {
            inner: Braid::from(stream).into(),
        }
    }
}

#[cfg(feature = "stream")]
impl From<DuplexStream> for Stream {
    fn from(stream: DuplexStream) -> Self {
        Stream {
            inner: Braid::from(stream).into(),
        }
    }
}

#[cfg(feature = "stream")]
impl From<UnixStream> for Stream {
    fn from(stream: UnixStream) -> Self {
        Stream {
            inner: Braid::from(stream).into(),
        }
    }
}

impl<IO> AsyncRead for Stream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    IO::Addr: Unpin,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.project().inner.poll_read(cx, buf)
    }
}

impl<IO> AsyncWrite for Stream<IO>
where
    IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    IO::Addr: Unpin,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        self.project().inner.poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        self.project().inner.poll_shutdown(cx)
    }
}
