//! Transport streams for connecting to remote servers.
//!
//! Transports are responsible for establishing a connection to a remote server, shuffling bytes back and forth,

use std::future::Future;
#[cfg(feature = "tls")]
use std::sync::Arc;

#[cfg(feature = "tls")]
use rustls::client::ClientConfig;
#[cfg(feature = "tls")]
use thiserror::Error;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tower::Service;

use self::oneshot::Oneshot;
#[cfg(feature = "stream")]
pub use self::stream::IntoStream;
#[cfg(feature = "tls")]
use self::tls::TlsTransportWrapper;
use crate::client::conn::Stream;
#[cfg(feature = "tls")]
use crate::client::default_tls_config;

#[cfg(feature = "stream")]
use crate::info::BraidAddr;
use crate::info::HasConnectionInfo;
use crate::IntoRequestParts;

#[cfg(feature = "mocks")]
pub mod mock;
#[cfg(feature = "stream")]
pub(crate) mod stream;
pub mod tcp;
#[cfg(feature = "tls")]
pub mod tls;
#[cfg(target_family = "unix")]
pub mod unix;

/// A transport provides data transmission between two endpoints.
///
/// To implement a transport stream, implement a [`tower::Service`] which accepts a URI and returns
/// an IO stream, which must be compatible with a [`super::Protocol`]. For example, HTTP protocols
/// require an IO stream which implements [`tokio::io::AsyncRead`] and [`tokio::io::AsyncWrite`].
pub trait Transport: Send {
    /// The type of IO stream used by this transport
    type IO: HasConnectionInfo + Send + 'static;

    /// Error returned when connection fails
    type Error: std::error::Error + Send + Sync + 'static;

    /// The future type returned by this service
    type Future: Future<Output = Result<Self::IO, <Self as Transport>::Error>> + Send + 'static;

    /// Connect to a remote server and return a stream.
    fn connect(&mut self, req: http::request::Parts) -> <Self as Transport>::Future;

    /// Poll the transport to see if it is ready to accept a new connection.
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Transport>::Error>>;
}

impl<T, IO> Transport for T
where
    T: Service<http::request::Parts, Response = IO>,
    T: Clone + Send + Sync + 'static,
    T::Error: std::error::Error + Send + Sync + 'static,
    T::Future: Send + 'static,
    IO: HasConnectionInfo + Send + 'static,
    IO::Addr: Send,
{
    type IO = IO;
    type Error = T::Error;
    type Future = T::Future;

    fn connect(
        &mut self,
        req: http::request::Parts,
    ) -> <Self as Service<http::request::Parts>>::Future {
        self.call(req.into_request_parts())
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Transport>::Error>> {
        Service::poll_ready(self, cx)
    }
}

/// A wrapper type which converts any service that accepts a URI
/// into a `hyperdriver` tranport type. Hyperdriver uses http::request::Parts
/// for transports, but many implementations only require the http::Uri
/// in order to function.
#[derive(Debug, Clone)]
pub struct UriTransport<T>(T);

impl<T, IO> Transport for UriTransport<T>
where
    T: Service<http::Uri, Response = IO>,
    T: Clone + Send + Sync + 'static,
    T::Error: std::error::Error + Send + Sync + 'static,
    T::Future: Send + 'static,
    IO: HasConnectionInfo + Send + 'static,
    IO::Addr: Send,
{
    type IO = IO;
    type Error = T::Error;
    type Future = T::Future;

    fn connect(&mut self, req: http::request::Parts) -> <Self as Transport>::Future {
        let parts = req.into_request_parts();
        self.0.call(parts.uri)
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Transport>::Error>> {
        Service::poll_ready(&mut self.0, cx)
    }
}

/// Extension trait for Transports to provide additional configuration options.
pub trait TransportExt: Transport {
    /// Connect to a remote server and return a stream.
    fn connect_with<R>(&mut self, req: R) -> <Self as Transport>::Future
    where
        R: IntoRequestParts,
    {
        self.connect(req.into_request_parts())
    }

    #[cfg(feature = "stream")]
    /// Wrap the transport in a converter which produces a Stream
    fn into_stream(self) -> IntoStream<Self>
    where
        Self::IO: Into<Stream> + AsyncRead + AsyncWrite + Unpin + Send + 'static,
        <<Self as Transport>::IO as HasConnectionInfo>::Addr: Into<BraidAddr>,
        Self: Sized,
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

    /// Create a future which uses the given transport to connect after calling poll_ready.
    fn oneshot<R>(self, request: R) -> Oneshot<Self, R>
    where
        Self: Sized,
        R: IntoRequestParts,
    {
        Oneshot::new(self, request)
    }
}

impl<T> TransportExt for T where T: Transport {}

/// An error returned when a TLS connection attempt fails
#[cfg(feature = "tls")]
#[derive(Debug, Error)]
#[non_exhaustive]
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

impl<T> Service<http::request::Parts> for TlsTransport<T>
where
    T: Transport,
    <T as Transport>::IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    <<T as Transport>::IO as HasConnectionInfo>::Addr: Clone + Send + Unpin,
{
    type Response = Stream<T::IO>;

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

    fn call(&mut self, parts: http::request::Parts) -> Self::Future {
        #[cfg_attr(not(feature = "tls"), allow(unused_variables))]
        let use_tls = parts
            .uri
            .scheme_str()
            .is_some_and(|s| matches!(s, "https" | "wss"));

        match &mut self.braid {
            InnerBraid::Plain(inner) => {
                tracing::trace!(scheme=?parts.uri.scheme_str(), "connecting without TLS");
                self::future::TransportBraidFuture::from_plain(inner.connect(parts))
            }
            #[cfg(feature = "tls")]
            InnerBraid::Tls(inner) if use_tls => {
                tracing::trace!(scheme=?parts.uri.scheme_str(), "connecting with TLS");
                self::future::TransportBraidFuture::from_tls(inner.call(parts))
            }
            #[cfg(feature = "tls")]
            InnerBraid::Tls(inner) => {
                tracing::trace!(scheme=?parts.uri.scheme_str(), "connecting without TLS");
                self::future::TransportBraidFuture::from_plain(inner.transport_mut().connect(parts))
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
        type Output = Result<super::Stream<T::IO>, super::TlsConnectionError<T::Error>>;

        #[cfg(not(feature = "tls"))]
        type Output = Result<super::Stream<T::IO>, T::Error>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            #[cfg(feature = "tls")]
            match self.project().inner.project() {
                InnerBraidFutureProj::Plain(fut) => fut
                    .poll(cx)
                    .map_ok(super::Stream::new)
                    .map_err(TlsConnectionError::Connection),
                InnerBraidFutureProj::Tls(fut) => fut.poll(cx),
            }

            #[cfg(not(feature = "tls"))]
            match self.project().inner.project() {
                InnerBraidFutureProj::Plain(fut) => fut.poll(cx).map_ok(super::Stream::new),
            }
        }
    }
}

mod oneshot {
    use std::{fmt, future::Future, task::ready};

    use crate::IntoRequestParts;

    use super::Transport;
    use super::TransportExt;

    #[pin_project::pin_project(project=OneshotStateProj)]
    enum OneshotState<T: Transport, R> {
        Pending { transport: T, request: Option<R> },
        Ready(#[pin] T::Future),
    }

    impl<T: Transport, R> fmt::Debug for OneshotState<T, R> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                OneshotState::Pending { .. } => f.debug_struct("OneshotState::Pending").finish(),
                OneshotState::Ready(_) => f.debug_struct("OneshotState::Ready").finish(),
            }
        }
    }

    #[derive(Debug)]
    #[pin_project::pin_project]
    pub struct Oneshot<T: Transport, R> {
        #[pin]
        state: OneshotState<T, R>,
    }

    impl<T, R> Oneshot<T, R>
    where
        T: Transport,
    {
        pub fn new(transport: T, request: R) -> Self {
            Self {
                state: OneshotState::Pending {
                    transport,
                    request: Some(request),
                },
            }
        }
    }

    impl<T, R> Future for Oneshot<T, R>
    where
        T: Transport,
        R: IntoRequestParts,
    {
        type Output = Result<T::IO, T::Error>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            let mut this = self.project();
            loop {
                match this.state.as_mut().project() {
                    OneshotStateProj::Pending { transport, request } => {
                        ready!(transport.poll_ready(cx))?;
                        let fut = transport.connect_with(request.take().unwrap());
                        this.state.set(OneshotState::Ready(fut));
                    }
                    OneshotStateProj::Ready(fut) => {
                        return fut.poll(cx);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::future::Ready;

    use static_assertions::assert_obj_safe;
    assert_obj_safe!(Transport<IO=(), Error=(), Future=Ready<Result<(),()>>>);

    #[cfg(feature = "stream")]
    mod stream {
        use super::*;

        use crate::{info::HasTlsConnectionInfo, stream::tcp::TcpStream};
        use static_assertions::assert_impl_all;

        assert_impl_all!(Stream: HasTlsConnectionInfo, HasConnectionInfo);
        assert_impl_all!(Stream: Send, Sync, Unpin);

        assert_impl_all!(TcpStream: HasConnectionInfo);
    }
}
