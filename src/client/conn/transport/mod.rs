//! Transport streams for connecting to remote servers.
//!
//! Transports are responsible for establishing a connection to a remote server, shuffling bytes back and forth,

use std::future::Future;
use std::io;
#[cfg(feature = "tls")]
use std::sync::Arc;

use ::http::Uri;
use pin_project::pin_project;
#[cfg(feature = "tls")]
use rustls::client::ClientConfig;
#[cfg(feature = "tls")]
use thiserror::Error;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tower::Service;

#[cfg(feature = "stream")]
pub use self::stream::IntoStream;
use crate::client::conn::Stream;
#[cfg(feature = "tls")]
use crate::client::default_tls_config;
use crate::client::pool::PoolableTransport;
#[cfg(feature = "tls")]
use crate::info::tls::HasTlsConnectionInfo;
#[cfg(feature = "stream")]
use crate::info::BraidAddr;
use crate::info::ConnectionInfo;
use crate::info::HasConnectionInfo;
#[cfg(feature = "tls")]
use crate::info::TlsConnectionInfo;
#[cfg(all(feature = "tls", feature = "stream"))]
use crate::stream::tls::TlsHandshakeStream as _;

#[cfg(feature = "tls")]
use self::tls::TlsTransportWrapper;

#[cfg(feature = "stream")]
pub mod duplex;
#[cfg(feature = "mocks")]
pub mod mock;
#[cfg(feature = "stream")]
pub(crate) mod stream;
pub mod tcp;
#[cfg(feature = "tls")]
pub mod tls;

/// A transport provides data transmission between two endpoints.
///
/// To implement a transport stream, implement a [`tower::Service`] which accepts a URI and returns a
/// [`TransportStream`]. [`TransportStream`] is a wrapper around an IO stream which provides additional
/// information about the connection, such as the remote address and the protocol being used. The underlying
/// IO stream must implement [`tokio::io::AsyncRead`] and [`tokio::io::AsyncWrite`].
pub trait Transport: Clone + Send {
    /// The type of IO stream used by this transport
    type IO: HasConnectionInfo + Send + 'static;

    /// Error returned when connection fails
    type Error: std::error::Error + Send + Sync + 'static;

    /// The future type returned by this service
    type Future: Future<Output = Result<TransportStream<Self::IO>, <Self as Transport>::Error>>
        + Send
        + 'static;

    /// Connect to a remote server and return a stream.
    fn connect(&mut self, uri: Uri) -> <Self as Transport>::Future;

    /// Poll the transport to see if it is ready to accept a new connection.
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Transport>::Error>>;
}

impl<T, IO> Transport for T
where
    T: Service<Uri, Response = TransportStream<IO>>,
    T: Clone + Send + Sync + 'static,
    T::Error: std::error::Error + Send + Sync + 'static,
    T::Future: Send + 'static,
    IO: HasConnectionInfo + Send + 'static,
    IO::Addr: Send,
{
    type IO = IO;
    type Error = T::Error;
    type Future = T::Future;

    fn connect(&mut self, uri: Uri) -> <Self as Service<Uri>>::Future {
        self.call(uri)
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Transport>::Error>> {
        Service::poll_ready(self, cx)
    }
}

/// Extension trait for Transports to provide additional configuration options.
pub trait TransportExt: Transport {
    #[cfg(feature = "stream")]
    /// Wrap the transport in a converter which produces a Stream
    fn into_stream(self) -> IntoStream<Self>
    where
        Self::IO: Into<Stream> + AsyncRead + AsyncWrite + Unpin + Send + 'static,
        <<Self as Transport>::IO as HasConnectionInfo>::Addr: Into<BraidAddr>,
    {
        IntoStream::new(self)
    }

    #[cfg(feature = "tls")]
    /// Wrap the transport in a TLS layer.
    fn with_tls(self, config: Arc<ClientConfig>) -> TlsTransport<Self>
    where
        Self: Sized,
    {
        TlsTransport::new(self).with_tls(config)
    }

    #[cfg(feature = "tls")]
    /// Wrap the transport in a TLS layer configured with a default client configuration.
    fn with_default_tls(self) -> TlsTransport<Self>
    where
        Self: Sized,
    {
        TlsTransport::new(self).with_default_tls()
    }

    /// Wrap the transport in a no-TLS layer.
    fn without_tls(self) -> TlsTransport<Self>
    where
        Self: Sized,
    {
        TlsTransport::new(self)
    }

    #[cfg(feature = "tls")]
    /// Wrap the transport in a TLS layer if the given config is `Some`, otherwise wrap it in a no-TLS layer.
    fn with_optional_tls(self, config: Option<Arc<ClientConfig>>) -> TlsTransport<Self>
    where
        Self: Sized,
    {
        match config {
            Some(config) => self.with_tls(config),
            None => self.without_tls(),
        }
    }

    #[cfg(not(feature = "tls"))]
    /// Wrap the transport in a no-TLS layer if the given config is `Some`, otherwise wrap it in a no-TLS layer.
    ///
    /// Since the `tls` feature is not enabled, this method will always wrap the transport in a no-TLS layer.
    fn with_optional_tls(self, config: Option<()>) -> TlsTransport<Self>
    where
        Self: Sized,
    {
        debug_assert!(config.is_none(), "TLS is not enabled");
        self.without_tls()
    }
}

impl<T> TransportExt for T where T: Transport {}

/// A wrapper around an IO stream which provides additional information about the connection.
///
/// This is used to attach [`ConnectionInfo`] to an arbitrary IO stream. IO streams must implement
/// [`tokio::io::AsyncRead`] and [`tokio::io::AsyncWrite`] to be functional.
#[derive(Debug)]
#[pin_project]
pub struct TransportStream<IO>
where
    IO: HasConnectionInfo,
{
    #[pin]
    stream: IO,
    info: ConnectionInfo<IO::Addr>,
    #[cfg(feature = "tls")]
    tls: Option<TlsConnectionInfo>,
}

impl<IO> TransportStream<IO>
where
    IO: HasConnectionInfo,
{
    /// Create a new transport from an IO stream.
    pub fn new(stream: IO, #[cfg(feature = "tls")] tls: Option<TlsConnectionInfo>) -> Self {
        let info = stream.info();

        #[cfg(feature = "tls")]
        return Self { stream, info, tls };

        #[cfg(not(feature = "tls"))]
        Self { stream, info }
    }
}

impl<IO> TransportStream<IO>
where
    IO: HasConnectionInfo,
{
    #[cfg(feature = "tls")]
    /// Get the connection information for the transport.
    pub fn tls_info(&self) -> Option<&TlsConnectionInfo> {
        #[cfg(feature = "tls")]
        return self.tls.as_ref();
    }

    /// Reduce the transport to its inner IO stream.
    pub fn into_inner(self) -> IO {
        self.stream
    }

    /// Get a reference to the inner IO stream.
    pub fn get_io_ref(&self) -> &IO {
        &self.stream
    }

    /// Get a mutable reference to the inner IO stream.
    pub fn get_io_mut(&mut self) -> &mut IO {
        &mut self.stream
    }

    /// Return the parts of the stream.
    pub fn into_parts(self) -> (IO, ConnectionInfo<IO::Addr>) {
        (self.stream, self.info)
    }

    /// Map the inner IO stream to a new IO stream.
    pub fn map<F, U>(self, f: F) -> TransportStream<U>
    where
        F: FnOnce(IO) -> U,
        U: HasConnectionInfo,
        IO::Addr: Into<U::Addr>,
    {
        TransportStream {
            stream: f(self.stream),
            info: self.info.map(Into::into),
            #[cfg(feature = "tls")]
            tls: self.tls,
        }
    }
}

#[cfg(feature = "stream")]
impl TransportStream<Stream> {
    /// Create a new transport from a `crate::client::stream::Stream`.
    #[cfg_attr(not(feature = "tls"), allow(unused_mut))]
    pub async fn new_stream(mut stream: Stream) -> io::Result<Self> {
        #[cfg(feature = "tls")]
        stream.finish_handshake().await?;

        let info = stream.info();

        #[cfg(feature = "tls")]
        let tls = stream.tls_info().cloned();

        Ok(Self {
            stream,
            info,
            #[cfg(feature = "tls")]
            tls,
        })
    }
}

impl<IO> PoolableTransport for TransportStream<IO>
where
    IO: HasConnectionInfo + Unpin + Send + 'static,
    IO::Addr: Send,
{
    #[cfg(feature = "tls")]
    fn can_share(&self) -> bool {
        self.tls.as_ref().and_then(|tls| tls.alpn.as_ref())
            == Some(&crate::info::Protocol::Http(::http::Version::HTTP_2))
    }

    #[cfg(not(feature = "tls"))]
    fn can_share(&self) -> bool {
        false
    }
}

impl<IO> HasConnectionInfo for TransportStream<IO>
where
    IO: HasConnectionInfo,
    <IO as HasConnectionInfo>::Addr: Clone,
{
    type Addr = IO::Addr;

    fn info(&self) -> ConnectionInfo<Self::Addr> {
        self.info.clone()
    }
}

#[cfg(feature = "tls")]
impl<IO> HasTlsConnectionInfo for TransportStream<IO>
where
    IO: HasTlsConnectionInfo,
    <IO as HasConnectionInfo>::Addr: Clone,
{
    fn tls_info(&self) -> Option<&TlsConnectionInfo> {
        self.tls.as_ref()
    }
}

impl<IO> AsyncRead for TransportStream<IO>
where
    IO: HasConnectionInfo + AsyncRead,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        self.project().stream.poll_read(cx, buf)
    }
}

impl<IO> AsyncWrite for TransportStream<IO>
where
    IO: HasConnectionInfo + AsyncWrite,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, io::Error>> {
        self.project().stream.poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), io::Error>> {
        self.project().stream.poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), io::Error>> {
        self.project().stream.poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_write_vectored(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> std::task::Poll<Result<usize, io::Error>> {
        self.project().stream.poll_write_vectored(cx, bufs)
    }
}

/// An error returned when a TLS connection attempt fails
#[cfg(feature = "tls")]
#[derive(Debug, Error)]
pub enum TlsConnectionError<E> {
    /// An error occured while trying to connect via the underlying transport
    /// before any TLS handshake was attempted.
    #[error(transparent)]
    Connection(#[from] E),

    /// An error occured during the TLS handshake.
    #[error("TLS handshake failed: {0}")]
    Handshake(#[source] std::io::Error),

    /// The request did not contain a domain, making TLS certificate verification impossible.
    #[error("No domain found in URI")]
    NoDomain,

    /// The TLS feature is disabled, but TLS was requested.
    #[error("TLS is not enabled, can't connect to https")]
    TlsDisabled,
}

#[derive(Debug, Clone)]
enum InnerBraid<T> {
    Plain(T),

    #[cfg(feature = "tls")]
    Tls(TlsTransportWrapper<T>),
}

/// A transport that can be used to connect to a remote server, with optional TLS support.
#[derive(Debug, Clone)]
pub struct TlsTransport<T> {
    braid: InnerBraid<T>,
}

impl<T: Default> Default for TlsTransport<T> {
    fn default() -> Self {
        #[cfg(feature = "tls")]
        return Self::new(T::default()).with_tls(default_tls_config().into());

        #[cfg(not(feature = "tls"))]
        Self::new(T::default())
    }
}

impl<T> TlsTransport<T> {
    /// A transport that can be used to connect to a remote server, with optional TLS support.
    pub fn new(transport: T) -> Self {
        Self {
            braid: InnerBraid::Plain(transport),
        }
    }

    #[cfg(feature = "tls")]
    /// Enable TLS on the transport with the given config.
    pub fn with_tls(self, config: Arc<ClientConfig>) -> Self {
        let inner = match self.braid {
            InnerBraid::Plain(inner) => inner,
            InnerBraid::Tls(transport) => transport.into_parts().0,
        };

        let transport = TlsTransportWrapper::new(inner, config);
        Self {
            braid: InnerBraid::Tls(transport),
        }
    }

    #[cfg(feature = "tls")]
    /// Enable TLS on the transport with the default configuration.
    pub fn with_default_tls(self) -> Self {
        let config = default_tls_config();
        self.with_tls(config.into())
    }

    /// Unwrap the inner IO stream.
    pub fn into_inner(self) -> T {
        match self.braid {
            InnerBraid::Plain(inner) => inner,
            #[cfg(feature = "tls")]
            InnerBraid::Tls(transport) => transport.into_parts().0,
        }
    }

    #[cfg(feature = "tls")]
    /// Unwrap the inner IO stream and the TLS config.
    pub fn into_parts(self) -> (T, Option<Arc<ClientConfig>>) {
        match self.braid {
            InnerBraid::Plain(inner) => (inner, None),
            InnerBraid::Tls(transport) => {
                let (stream, config) = transport.into_parts();
                (stream, Some(config))
            }
        }
    }

    #[cfg(feature = "tls")]
    /// Get the TLS config.
    pub fn tls_config(&self) -> Option<&Arc<ClientConfig>> {
        match &self.braid {
            InnerBraid::Plain(_) => None,
            InnerBraid::Tls(transport) => Some(transport.config()),
        }
    }

    /// Get a reference to the inner IO stream.
    pub fn inner(&self) -> &T {
        match &self.braid {
            InnerBraid::Plain(inner) => inner,
            #[cfg(feature = "tls")]
            InnerBraid::Tls(transport) => transport.transport(),
        }
    }

    /// Get a mutable reference to the inner IO stream.
    pub fn inner_mut(&mut self) -> &mut T {
        match &mut self.braid {
            InnerBraid::Plain(inner) => inner,
            #[cfg(feature = "tls")]
            InnerBraid::Tls(transport) => transport.transport_mut(),
        }
    }
}

impl<T> Service<Uri> for TlsTransport<T>
where
    T: Transport,
    <T as Transport>::IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    <<T as Transport>::IO as HasConnectionInfo>::Addr: Clone + Send + Unpin,
{
    type Response = TransportStream<Stream<T::IO>>;

    #[cfg(feature = "tls")]
    type Error = TlsConnectionError<T::Error>;

    #[cfg(not(feature = "tls"))]
    type Error = T::Error;

    type Future = self::future::TransportBraidFuture<T>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        #[cfg(feature = "tls")]
        match &mut self.braid {
            InnerBraid::Plain(inner) => {
                inner.poll_ready(cx).map_err(TlsConnectionError::Connection)
            }
            InnerBraid::Tls(inner) => inner.poll_ready(cx),
        }

        #[cfg(not(feature = "tls"))]
        match &mut self.braid {
            InnerBraid::Plain(inner) => inner.poll_ready(cx),
        }
    }

    fn call(&mut self, req: Uri) -> Self::Future {
        #[cfg_attr(not(feature = "tls"), allow(unused_variables))]
        let use_tls = req
            .scheme_str()
            .is_some_and(|s| matches!(s, "https" | "wss"));

        match &mut self.braid {
            InnerBraid::Plain(inner) => {
                tracing::trace!(scheme=?req.scheme_str(), "connecting without TLS");
                self::future::TransportBraidFuture::from_plain(inner.connect(req))
            }
            #[cfg(feature = "tls")]
            InnerBraid::Tls(inner) if use_tls => {
                tracing::trace!(scheme=?req.scheme_str(), "connecting with TLS");
                self::future::TransportBraidFuture::from_tls(inner.call(req))
            }
            #[cfg(feature = "tls")]
            InnerBraid::Tls(inner) => {
                tracing::trace!(scheme=?req.scheme_str(), "connecting without TLS");
                self::future::TransportBraidFuture::from_plain(inner.transport_mut().connect(req))
            }
        }
    }
}

mod future {
    use std::{fmt, future::Future};

    use pin_project::pin_project;
    use tokio::io::{AsyncRead, AsyncWrite};

    use crate::info::HasConnectionInfo;

    #[cfg(feature = "tls")]
    use super::TlsConnectionError;
    use super::Transport;

    #[pin_project(project=InnerBraidFutureProj)]
    pub(super) enum InnerBraidFuture<T>
    where
        T: Transport,
    {
        Plain(#[pin] T::Future),

        #[cfg(feature = "tls")]
        Tls(#[pin] super::tls::future::TlsConnectionFuture<T>),
    }

    impl<T: Transport> fmt::Debug for InnerBraidFuture<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                InnerBraidFuture::Plain(_) => f.debug_struct("Braid::Plain").finish(),

                #[cfg(feature = "tls")]
                InnerBraidFuture::Tls(_) => f.debug_struct("Braid::Tls").finish(),
            }
        }
    }

    #[pin_project]
    #[derive(Debug)]
    pub struct TransportBraidFuture<T>
    where
        T: Transport,
    {
        #[pin]
        inner: InnerBraidFuture<T>,
    }

    impl<T> TransportBraidFuture<T>
    where
        T: Transport,
    {
        pub(super) fn from_plain(fut: T::Future) -> Self {
            Self {
                inner: InnerBraidFuture::Plain(fut),
            }
        }

        #[cfg(feature = "tls")]
        pub(super) fn from_tls(fut: super::tls::future::TlsConnectionFuture<T>) -> Self {
            Self {
                inner: InnerBraidFuture::Tls(fut),
            }
        }
    }

    impl<T> Future for TransportBraidFuture<T>
    where
        T: Transport,
        <T as Transport>::IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
        <<T as Transport>::IO as HasConnectionInfo>::Addr: Clone + Send + Unpin,
    {
        #[cfg(feature = "tls")]
        type Output = Result<
            super::TransportStream<super::Stream<T::IO>>,
            super::TlsConnectionError<T::Error>,
        >;

        #[cfg(not(feature = "tls"))]
        type Output = Result<super::TransportStream<super::Stream<T::IO>>, T::Error>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            #[cfg(feature = "tls")]
            match self.project().inner.project() {
                InnerBraidFutureProj::Plain(fut) => fut
                    .poll(cx)
                    .map_ok(|ts| ts.map(super::Stream::new))
                    .map_err(TlsConnectionError::Connection),
                InnerBraidFutureProj::Tls(fut) => fut.poll(cx),
            }

            #[cfg(not(feature = "tls"))]
            match self.project().inner.project() {
                InnerBraidFutureProj::Plain(fut) => {
                    fut.poll(cx).map_ok(|ts| ts.map(super::Stream::new))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use static_assertions::assert_impl_all;
    use tokio::net::TcpStream;

    assert_impl_all!(TransportStream<Stream>: HasTlsConnectionInfo, HasConnectionInfo);
    assert_impl_all!(TransportStream<Stream>: Send, Sync, Unpin);

    assert_impl_all!(TransportStream<TcpStream>: HasConnectionInfo);
}
