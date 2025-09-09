//! DNS resolution utilities.

#[cfg(feature = "tls")]
use std::marker::PhantomData;
use std::net::{IpAddr, ToSocketAddrs};
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::{fmt, io};

use chateau::client::conn::dns::{Resolver, SocketAddrs};
#[cfg(feature = "tls")]
use chateau::client::conn::transport::TlsAddr;
use futures_util::Future;
use pin_project::{pin_project, pinned_drop};
use tokio::task::JoinHandle;

#[cfg(feature = "tls")]
#[derive(Debug, thiserror::Error)]
pub enum TlsResolverError<E> {
    #[error("{0}")]
    Resolver(#[source] E),
    #[error("No hostname in URI for TLS: {0}")]
    MissingHostname(http::Uri),
}

#[cfg(feature = "tls")]
#[derive(Debug, Default, Clone)]
pub struct TlsResolver<R> {
    inner: R,
}

#[cfg(feature = "tls")]
impl<R> TlsResolver<R> {
    pub fn new(resolver: R) -> Self {
        Self { inner: resolver }
    }
}

#[cfg(feature = "tls")]
impl<R, B, A> tower::Service<&http::Request<B>> for TlsResolver<R>
where
    R: Resolver<http::Request<B>, Address = A>,
{
    type Response = TlsAddr<A>;
    type Error = TlsResolverError<R::Error>;
    type Future = TlsResolverFuture<R::Future, A, R::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner
            .poll_ready(cx)
            .map_err(TlsResolverError::Resolver)
    }

    fn call(&mut self, req: &http::Request<B>) -> Self::Future {
        let Some(hostname) = req.uri().host().map(|s| s.to_owned()) else {
            return TlsResolverFuture::missing_hostname(req.uri().clone());
        };

        TlsResolverFuture::new(self.inner.resolve(req), hostname)
    }
}

#[cfg(feature = "tls")]
#[pin_project(project = TlsResolverFutureStateProj)]
enum TlsResolverFutureState<F> {
    Future {
        #[pin]
        future: F,
        hostname: Option<String>,
    },

    MissingHostname(Option<http::Uri>),
}

#[cfg(feature = "tls")]
#[pin_project]
pub struct TlsResolverFuture<F, A, E> {
    #[pin]
    state: TlsResolverFutureState<F>,
    address: PhantomData<fn() -> (A, E)>,
}

#[cfg(feature = "tls")]
impl<F, A, E> TlsResolverFuture<F, A, E> {
    fn missing_hostname(err: http::Uri) -> Self {
        Self {
            state: TlsResolverFutureState::MissingHostname(Some(err)),
            address: PhantomData,
        }
    }

    fn new(future: F, hostname: String) -> Self {
        Self {
            state: TlsResolverFutureState::Future {
                future,
                hostname: Some(hostname),
            },
            address: PhantomData,
        }
    }
}

#[cfg(feature = "tls")]
impl<F, A, E> Future for TlsResolverFuture<F, A, E>
where
    F: Future<Output = Result<A, E>>,
{
    type Output = Result<TlsAddr<A>, TlsResolverError<E>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.as_mut().project().state.project() {
            TlsResolverFutureStateProj::Future { future, hostname } => match future.poll(cx) {
                Poll::Ready(Ok(addr)) => Poll::Ready(Ok(TlsAddr::new(
                    addr,
                    hostname
                        .take()
                        .expect("TlsResolverFuture polled after ready"),
                ))),
                Poll::Ready(Err(err)) => Poll::Ready(Err(TlsResolverError::Resolver(err))),
                Poll::Pending => Poll::Pending,
            },
            TlsResolverFutureStateProj::MissingHostname(uri) => {
                Poll::Ready(Err(TlsResolverError::MissingHostname(
                    uri.take().expect("TlsResolveFuture polled after error"),
                )))
            }
        }
    }
}

/// GetAddrInfo based resolver.
///
/// This resolver uses the `getaddrinfo` system call to resolve
/// hostnames to IP addresses via the operating system.
#[derive(Debug, Default, Clone)]
pub struct GaiResolver {
    _priv: (),
}

impl GaiResolver {
    /// Create a new `GaiResolver`.
    pub fn new() -> Self {
        Self { _priv: () }
    }
}

impl<B> tower::Service<&http::Request<B>> for GaiResolver {
    type Response = SocketAddrs;
    type Error = io::Error;
    type Future = GaiFuture;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: &http::Request<B>) -> Self::Future {
        let span = tracing::Span::current();
        let (host, port) = match get_host_and_port(req.uri()) {
            Ok((host, port)) => (host, port),
            Err(e) => return GaiFuture::error(e),
        };
        GaiFuture::handle(JoinHandleFuture {
            handle: tokio::task::spawn_blocking(move || {
                tracing::trace_span!(parent: &span, "getaddrinfo").in_scope(|| {
                    tracing::trace!("dns resolution starting");
                    (host.as_ref(), port)
                        .to_socket_addrs()
                        .map(SocketAddrs::from_iter)
                })
            }),
        })
    }
}

fn get_host_and_port(uri: &http::Uri) -> Result<(Box<str>, u16), io::Error> {
    let host = uri.host().ok_or(io::Error::new(
        io::ErrorKind::InvalidData,
        "missing host in URI",
    ))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port = match uri.port_u16() {
        Some(port) => port,
        None => match uri.scheme_str() {
            Some("http") => 80,
            Some("https") => 443,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "missing port in URI for non-http(s) scheme",
                ))
            }
        },
    };

    Ok((host.into(), port))
}

pub(crate) fn resolve<A: ToSocketAddrs + Send + 'static>(addr: A) -> JoinHandleFuture<SocketAddrs> {
    let span = tracing::Span::current();
    JoinHandleFuture {
        handle: tokio::task::spawn_blocking(move || {
            tracing::trace_span!(parent: &span, "getaddrinfo").in_scope(|| {
                tracing::trace!("dns resolution starting");
                addr.to_socket_addrs().map(SocketAddrs::from_iter)
            })
        }),
    }
}

/// Future returned by `GaiResolver` when resolving
/// via getaddrinfo.
#[pin_project(PinnedDrop)]
pub struct JoinHandleFuture<Addr> {
    #[pin]
    handle: JoinHandle<Result<Addr, io::Error>>,
}

impl<Addr> fmt::Debug for JoinHandleFuture<Addr> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GaiFuture").finish()
    }
}

impl<Addr> Future for JoinHandleFuture<Addr> {
    type Output = Result<Addr, io::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match ready!(self.project().handle.poll(cx)) {
            Ok(Ok(addrs)) => Poll::Ready(Ok(addrs)),
            Ok(Err(error)) => Poll::Ready(Err(error)),
            Err(join_err) => {
                if join_err.is_cancelled() {
                    Poll::Ready(Err(io::Error::new(io::ErrorKind::Interrupted, join_err)))
                } else {
                    Poll::Ready(Err(io::Error::other(join_err)))
                }
            }
        }
    }
}

#[pinned_drop]
impl<Addr> PinnedDrop for JoinHandleFuture<Addr> {
    fn drop(self: Pin<&mut Self>) {
        self.handle.abort()
    }
}

/// A future returned by `GaiResolver` when resolving via getaddrinfo
/// in a worker thread.
#[derive(Debug)]
#[pin_project]
pub struct GaiFuture {
    #[pin]
    state: GaiFutureState,
}

impl GaiFuture {
    fn error(error: io::Error) -> Self {
        GaiFuture {
            state: GaiFutureState::Error(Some(error)),
        }
    }

    fn handle(handle: JoinHandleFuture<SocketAddrs>) -> Self {
        GaiFuture {
            state: GaiFutureState::JoinHandle(handle),
        }
    }
}

#[derive(Debug)]
#[pin_project(project = GaiFutureStateProj)]
enum GaiFutureState {
    JoinHandle(#[pin] JoinHandleFuture<SocketAddrs>),
    Error(Option<io::Error>),
}

impl Future for GaiFuture {
    type Output = Result<SocketAddrs, io::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        match this.state.project() {
            GaiFutureStateProj::JoinHandle(handle) => handle.poll(cx),
            GaiFutureStateProj::Error(err) => {
                Poll::Ready(Err(err.take().expect("error polled multiple times")))
            }
        }
    }
}

/// Converts a standard resolver (which can return multiple addresses)
/// into a resolver that only returns the first address as an IP address,
/// suitable for use with a TCP transport that doesn't do multiple connections.
///
/// Usually this is used in combination with a SimpleTcpTransport, which does
/// not support multiple connection attempts or the happy-eyeballs algorithm.
/// It might be approriate for internal connections where the host is known
/// to be a single address, or where load balancing is already handled.
#[derive(Debug, Clone)]
pub struct FirstAddrResolver<R> {
    inner: R,
}

impl<R> FirstAddrResolver<R> {
    /// Create a new `FirstAddrResolver`.
    pub fn new(inner: R) -> Self {
        Self { inner }
    }
}

impl<D, R> tower::Service<R> for FirstAddrResolver<D>
where
    D: tower::Service<R, Response = SocketAddrs, Error = io::Error>,
{
    type Response = IpAddr;
    type Error = io::Error;
    type Future = FirstAddrFuture<D::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: R) -> Self::Future {
        FirstAddrFuture {
            inner: self.inner.call(req),
        }
    }
}

/// Future returned by `FirstAddrResolver` when resolving
#[pin_project]
#[derive(Debug)]
pub struct FirstAddrFuture<F> {
    #[pin]
    inner: F,
}

impl<F> Future for FirstAddrFuture<F>
where
    F: Future<Output = Result<SocketAddrs, io::Error>>,
{
    type Output = Result<IpAddr, io::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().inner.poll(cx) {
            Poll::Ready(Ok(addrs)) => addrs.into_iter().next().map_or_else(
                || {
                    Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::AddrNotAvailable,
                        "no address found",
                    )))
                },
                |addr| Poll::Ready(Ok(addr.ip())),
            ),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Extension trait for resolvers that can be converted to a `FirstAddrResolver`.
pub trait FirstAddrExt {
    /// Convert this resolver into a `FirstAddrResolver`.
    ///
    /// This will return the first address resolved by the inner resolver,
    /// discarding the rest.
    fn first_addr(self) -> FirstAddrResolver<Self>
    where
        Self: Sized,
    {
        FirstAddrResolver::new(self)
    }
}

impl<R> FirstAddrExt for R {}
