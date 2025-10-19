//! Auto-TLS implementation
//!

#[cfg(feature = "tls")]
use std::sync::Arc;

#[cfg(feature = "tls")]
use chateau::client::conn::transport::TlsConnectionError;
use chateau::client::conn::transport::TlsRequest;
use chateau::client::conn::Transport;
#[cfg(feature = "tls")]
use rustls::client::ClientConfig;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use crate::client::conn::Stream;
#[cfg(feature = "tls")]
use crate::client::default_tls_config;
#[cfg(feature = "tls")]
use chateau::client::conn::transport::tls::TlsTransport;
use chateau::info::HasConnectionInfo;

/// A transport which requires TLS over the connection regardless of the protocol settings.
///
/// This is useful for prior knowledge requests, or when you want to ensure that a valid TLS connection
/// is used.
#[derive(Debug, Clone)]
pub struct HttpTlsTransport<T> {
    transport: TlsTransport<T>,
}

impl<T> HttpTlsTransport<T> {
    /// A transport that applies TLS with HTTP semantics for identifying the target hostname
    pub fn new(transport: T, config: Arc<ClientConfig>) -> Self {
        Self {
            transport: TlsTransport::new(transport, config),
        }
    }
}

impl<B, T> tower::Service<&http::Request<B>> for HttpTlsTransport<T>
where
    T: Transport<http::Request<B>> + Sync + Send + 'static,
    <T as Transport<http::Request<B>>>::IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    <<T as Transport<http::Request<B>>>::IO as HasConnectionInfo>::Addr: Clone + Send + Unpin,
{
    type Response = Stream<T::IO>;

    type Error = TlsConnectionError<T::Error>;

    type Future = self::future::TransportBraidFuture<T, http::Request<B>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        tower::Service::poll_ready(&mut self.transport, cx)
    }

    fn call(&mut self, request: &http::Request<B>) -> Self::Future {
        let tls_domain = get_tls_domain(request);

        let future = self.transport.call(TlsRequest::new(request, tls_domain));
        self::future::TransportBraidFuture::from_tls(future)
    }
}

#[derive(Debug, Clone)]
enum InnerBraid<T> {
    Plain(T),

    #[cfg(feature = "tls")]
    Tls(TlsTransport<T>),
}

/// A transport that can be used to connect to a remote server, with optional TLS support.
#[derive(Debug, Clone)]
pub struct AutoTlsTransport<T> {
    braid: InnerBraid<T>,
}

impl<T: Default> Default for AutoTlsTransport<T> {
    fn default() -> Self {
        #[cfg(feature = "tls")]
        return Self::new(T::default()).with_tls(default_tls_config().into());

        #[cfg(not(feature = "tls"))]
        Self::new(T::default())
    }
}

impl<T> AutoTlsTransport<T> {
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

        let transport = TlsTransport::new(inner, config);
        Self {
            braid: InnerBraid::Tls(transport),
        }
    }

    #[cfg(feature = "tls")]
    /// Optionally enable TLS on the transport with the given config.
    pub fn with_optional_tls(self, config: Option<Arc<ClientConfig>>) -> Self {
        match config {
            Some(config) => self.with_tls(config),
            None => self,
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

impl<B, T> tower::Service<&http::Request<B>> for AutoTlsTransport<T>
where
    T: Transport<http::Request<B>> + Sync + Send + 'static,
    <T as Transport<http::Request<B>>>::IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
    <<T as Transport<http::Request<B>>>::IO as HasConnectionInfo>::Addr: Clone + Send + Unpin,
{
    type Response = Stream<T::IO>;

    #[cfg(feature = "tls")]
    type Error = TlsConnectionError<T::Error>;

    #[cfg(not(feature = "tls"))]
    type Error = T::Error;

    type Future = self::future::TransportBraidFuture<T, http::Request<B>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        #[cfg(feature = "tls")]
        match &mut self.braid {
            InnerBraid::Plain(inner) => {
                inner.poll_ready(cx).map_err(TlsConnectionError::Connection)
            }
            InnerBraid::Tls(ref mut inner) => tower::Service::poll_ready(inner, cx),
        }

        #[cfg(not(feature = "tls"))]
        match &mut self.braid {
            InnerBraid::Plain(inner) => inner.poll_ready(cx),
        }
    }

    fn call(&mut self, request: &http::Request<B>) -> Self::Future {
        let tls_domain = get_tls_domain(request);

        match (&mut self.braid, tls_domain) {
            (InnerBraid::Plain(inner), _) => {
                tracing::trace!("connecting without TLS");
                self::future::TransportBraidFuture::from_plain(inner.connect(request))
            }
            #[cfg(feature = "tls")]
            (InnerBraid::Tls(inner), Some(domain)) => {
                tracing::trace!("connecting with TLS");
                self::future::TransportBraidFuture::from_tls(
                    inner.call(TlsRequest::new(request, Some(domain))),
                )
            }
            #[cfg(feature = "tls")]
            (InnerBraid::Tls(inner), None) => {
                tracing::trace!("connecting without TLS");
                self::future::TransportBraidFuture::from_plain(
                    inner.transport_mut().connect(request),
                )
            }
        }
    }
}

fn get_host<B>(request: &http::Request<B>) -> Option<String> {
    request
        .headers()
        .get(http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .or_else(|| request.uri().host())
        .map(|h| h.to_owned())
}

pub(in crate::client) fn get_tls_domain<B>(request: &http::Request<B>) -> Option<String> {
    //TODO: Allow overriding configuration here via request extensions

    match request.uri().scheme() {
        Some(scheme) if *scheme == http::uri::Scheme::HTTPS => get_host(request),
        Some(scheme) if scheme.as_str() == "wss" => get_host(request),
        Some(_) => None,
        None => None,
    }
}

pub(in crate::client) mod future {
    use std::{fmt, future::Future};

    use pin_project::pin_project;
    use tokio::io::{AsyncRead, AsyncWrite};

    use chateau::info::HasConnectionInfo;

    #[cfg(feature = "tls")]
    use super::TlsConnectionError;
    use chateau::client::conn::Transport;

    #[pin_project(project=InnerBraidFutureProj)]
    pub(super) enum InnerBraidFuture<T, R>
    where
        T: Transport<R>,
    {
        Plain(#[pin] T::Future),

        #[cfg(feature = "tls")]
        Tls(#[pin] chateau::client::conn::transport::tls::future::TlsConnectionFuture<T, R>),
    }

    impl<R, T: Transport<R>> fmt::Debug for InnerBraidFuture<T, R> {
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
    pub struct TransportBraidFuture<T, R>
    where
        T: Transport<R>,
    {
        #[pin]
        inner: InnerBraidFuture<T, R>,
    }

    impl<T, R> TransportBraidFuture<T, R>
    where
        T: Transport<R>,
    {
        pub(super) fn from_plain(fut: T::Future) -> Self {
            Self {
                inner: InnerBraidFuture::Plain(fut),
            }
        }

        #[cfg(feature = "tls")]
        pub(super) fn from_tls(
            fut: chateau::client::conn::transport::tls::future::TlsConnectionFuture<T, R>,
        ) -> Self {
            Self {
                inner: InnerBraidFuture::Tls(fut),
            }
        }
    }

    impl<T, R> Future for TransportBraidFuture<T, R>
    where
        T: Transport<R>,
        <T as Transport<R>>::IO: HasConnectionInfo + AsyncRead + AsyncWrite + Unpin,
        <<T as Transport<R>>::IO as HasConnectionInfo>::Addr: Clone + Send + Unpin,
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
                InnerBraidFutureProj::Tls(fut) => fut.poll(cx).map_ok(|tls_stream| {
                    use chateau::stream::tls::OptTlsStream;

                    use crate::client::conn::Stream;

                    Stream::from(OptTlsStream::Tls(tls_stream))
                }),
            }

            #[cfg(not(feature = "tls"))]
            match self.project().inner.project() {
                InnerBraidFutureProj::Plain(fut) => fut.poll(cx).map_ok(super::Stream::new),
            }
        }
    }
}
