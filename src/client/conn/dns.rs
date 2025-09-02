//! DNS resolution utilities.

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::{fmt, io};

use futures_util::Future;
use pin_project::{pin_project, pinned_drop};
use tokio::task::JoinHandle;

/// A collection of socket addresses.
#[derive(Debug, Clone, Default)]
pub struct SocketAddrs(VecDeque<SocketAddr>);

impl SocketAddrs {
    pub(crate) fn set_port(&mut self, port: u16) {
        for addr in &mut self.0 {
            addr.set_port(port)
        }
    }

    pub(crate) fn pop(&mut self) -> Option<SocketAddr> {
        self.0.pop_front()
    }

    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    pub(crate) fn sort_preferred(&mut self, prefer: Option<IpVersion>) {
        let mut v4_idx = None;
        let mut v6_idx = None;

        for (idx, addr) in self.0.iter().enumerate() {
            match (addr.version(), v4_idx, v6_idx) {
                (IpVersion::V4, None, _) => {
                    v4_idx = Some(idx);
                }
                (IpVersion::V6, _, None) => {
                    v6_idx = Some(idx);
                }
                (_, Some(_), Some(_)) => break,
                _ => {}
            }
        }

        let v4: Option<SocketAddr>;
        let v6: Option<SocketAddr>;
        if v4_idx.zip(v6_idx).is_some_and(|(v4, v6)| v4 > v6) {
            v4 = v4_idx.and_then(|idx| self.0.remove(idx));
            v6 = v6_idx.and_then(|idx| self.0.remove(idx));
        } else {
            v6 = v6_idx.and_then(|idx| self.0.remove(idx));
            v4 = v4_idx.and_then(|idx| self.0.remove(idx));
        }

        match (prefer, v4, v6) {
            (Some(IpVersion::V4), Some(addr_v4), Some(addr_v6)) => {
                self.0.push_front(addr_v6);
                self.0.push_front(addr_v4);
            }
            (Some(IpVersion::V6), Some(addr_v4), Some(addr_v6)) => {
                self.0.push_front(addr_v4);
                self.0.push_front(addr_v6);
            }

            (_, Some(addr_v4), Some(addr_v6)) => {
                self.0.push_front(addr_v4);
                self.0.push_front(addr_v6);
            }
            (_, Some(addr_v4), None) => {
                self.0.push_front(addr_v4);
            }
            (_, None, Some(addr_v6)) => {
                self.0.push_front(addr_v6);
            }
            _ => {}
        }
    }
}

impl FromIterator<SocketAddr> for SocketAddrs {
    fn from_iter<T: IntoIterator<Item = SocketAddr>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl IntoIterator for SocketAddrs {
    type Item = SocketAddr;
    type IntoIter = std::collections::vec_deque::IntoIter<SocketAddr>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a SocketAddrs {
    type Item = &'a SocketAddr;
    type IntoIter = std::collections::vec_deque::Iter<'a, SocketAddr>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Extension trait for `IpAddr` and `SocketAddr` to get the IP version.
pub trait IpVersionExt {
    /// Get the IP version of this address.
    fn version(&self) -> IpVersion;
}

/// IP version.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum IpVersion {
    /// IPv4
    V4,

    /// IPv6
    V6,
}

impl IpVersion {
    pub(super) fn from_binding(
        ip_v4_address: Option<Ipv4Addr>,
        ip_v6_address: Option<Ipv6Addr>,
    ) -> Option<Self> {
        match (ip_v4_address, ip_v6_address) {
            // Prefer IPv6 if both are available.
            (Some(_), Some(_)) => Some(Self::V6),
            (Some(_), None) => Some(Self::V4),
            (None, Some(_)) => Some(Self::V6),
            (None, None) => None,
        }
    }

    /// Is this IP version IPv4?
    #[allow(dead_code)]
    pub fn is_v4(&self) -> bool {
        matches!(self, Self::V4)
    }

    /// Is this IP version IPv6?
    #[allow(dead_code)]
    pub fn is_v6(&self) -> bool {
        matches!(self, Self::V6)
    }
}

impl IpVersionExt for SocketAddr {
    fn version(&self) -> IpVersion {
        match self {
            SocketAddr::V4(_) => IpVersion::V4,
            SocketAddr::V6(_) => IpVersion::V6,
        }
    }
}

impl IpVersionExt for IpAddr {
    fn version(&self) -> IpVersion {
        match self {
            IpAddr::V4(_) => IpVersion::V4,
            IpAddr::V6(_) => IpVersion::V6,
        }
    }
}

/// GetAddrInfo based resolver.
///
/// This resolver uses the `getaddrinfo` system call to resolve
/// hostnames to IP addresses via the operating system.
#[derive(Debug, Default, Clone, Copy)]
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
            Err(e) => {
                return JoinHandleFuture {
                    handle: tokio::task::spawn_blocking(move || Err(e)),
                }
            }
        };
        JoinHandleFuture {
            handle: tokio::task::spawn_blocking(move || {
                tracing::trace_span!(parent: &span, "getaddrinfo").in_scope(|| {
                    tracing::trace!("dns resolution starting");
                    (host.as_ref(), port)
                        .to_socket_addrs()
                        .map(SocketAddrs::from_iter)
                })
            }),
        }
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

pub(crate) fn resolve<A: ToSocketAddrs + Send + 'static>(addr: A) -> GaiFuture {
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
pub type GaiFuture = JoinHandleFuture<SocketAddrs>;

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
            Poll::Ready(Ok(addrs)) => addrs.0.into_iter().next().map_or_else(
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
