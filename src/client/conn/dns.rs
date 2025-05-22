//! DNS resolution utilities.

use std::collections::VecDeque;
use std::future::Ready;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::{fmt, io};

use futures_util::{Future, FutureExt as _};
use pin_project::{pin_project, pinned_drop};
use tokio::task::JoinHandle;

use crate::BoxFuture;

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

impl tower::Service<Box<str>> for GaiResolver {
    type Response = SocketAddrs;
    type Error = io::Error;
    type Future = GaiFuture;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, host: Box<str>) -> Self::Future {
        let span = tracing::Span::current();
        JoinHandleFuture {
            handle: tokio::task::spawn_blocking(move || {
                tracing::trace_span!(parent: &span, "getaddrinfo").in_scope(|| {
                    tracing::trace!("dns resolution starting");
                    (host.as_ref(), 0)
                        .to_socket_addrs()
                        .map(SocketAddrs::from_iter)
                })
            }),
        }
    }
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

impl<R> tower::Service<Box<str>> for FirstAddrResolver<R>
where
    R: tower::Service<Box<str>, Response = SocketAddrs, Error = io::Error>,
{
    type Response = IpAddr;
    type Error = io::Error;
    type Future = FirstAddrFuture<R::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, host: Box<str>) -> Self::Future {
        FirstAddrFuture {
            inner: self.inner.call(host),
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

/// A resolver that parses the input hostname as an IP address.
#[derive(Debug, Clone, Default)]
pub struct ParseAddressResolver {
    _priv: (),
}

impl ParseAddressResolver {
    /// Create a new `ParseAddressResolver`.
    pub fn new() -> Self {
        Self { _priv: () }
    }
}

impl tower::Service<Box<str>> for ParseAddressResolver {
    type Response = SocketAddrs;
    type Error = io::Error;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, host: Box<str>) -> Self::Future {
        match host.parse::<IpAddr>() {
            Ok(addr) => std::future::ready(Ok(SocketAddrs::from_iter(std::iter::once(
                SocketAddr::new(addr, 0),
            )))),
            Err(e) => std::future::ready(Err(io::Error::new(io::ErrorKind::InvalidInput, e))),
        }
    }
}

/// A resolver that always returns the same address.
#[derive(Debug, Clone)]
pub struct ConstantResolver {
    addr: SocketAddr,
}

impl ConstantResolver {
    /// Create a new `ConstantResolver`.
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr }
    }
}

impl tower::Service<Box<str>> for ConstantResolver {
    type Response = SocketAddrs;
    type Error = io::Error;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: Box<str>) -> Self::Future {
        std::future::ready(Ok(SocketAddrs::from_iter(std::iter::once(self.addr))))
    }
}

/// Extension trait for resolvers.
pub trait ResolverExt {
    /// Address return type of the resolver.
    type Address;

    /// Resolve a hostname to an address.
    fn resolve(
        &mut self,
        host: Box<str>,
        timeout: Option<std::time::Duration>,
    ) -> BoxFuture<'static, Result<Self::Address, io::Error>> {
        match timeout {
            Some(timeout) => self.resolve_with_timeout(host, timeout),
            None => self.resolve_without_timeout(host),
        }
    }

    /// Resolve a hostname to an address with a timeout.
    fn resolve_with_timeout(
        &mut self,
        host: Box<str>,
        timeout: std::time::Duration,
    ) -> BoxFuture<'static, Result<Self::Address, io::Error>>;

    /// Resolve a hostname to an address without a timeout.
    fn resolve_without_timeout(
        &mut self,
        host: Box<str>,
    ) -> BoxFuture<'static, Result<Self::Address, io::Error>>;
}

impl<R> ResolverExt for R
where
    R: tower::Service<Box<str>, Error = io::Error>,
    R::Future: Send + 'static,
{
    type Address = R::Response;

    fn resolve_with_timeout(
        &mut self,
        host: Box<str>,
        timeout: std::time::Duration,
    ) -> BoxFuture<'static, Result<Self::Address, io::Error>> {
        let resolve = self.call(host);
        async move {
            tracing::trace!(?timeout, "resolve with timeout");
            let outcome = tokio::time::timeout(timeout, resolve).await;
            tracing::trace!(success=%outcome.is_ok(), "resolved dns");
            match outcome {
                Ok(Ok(addrs)) => Ok(addrs),
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("dns resolution timed out after {}ms", timeout.as_millis()),
                )),
                Ok(Err(error)) => Err(error),
            }
        }
        .boxed()
    }

    fn resolve_without_timeout(
        &mut self,
        host: Box<str>,
    ) -> BoxFuture<'static, Result<Self::Address, io::Error>> {
        self.call(host).boxed()
    }
}

#[cfg(test)]
mod test {

    use super::*;

    #[tokio::test]
    async fn constant_resolver() {
        let mut resolver = ConstantResolver::new(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 80));
        let mut addrs = resolver
            .resolve_without_timeout("localhost".into())
            .await
            .unwrap();
        assert_eq!(addrs.pop().unwrap().ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));

        let resolve_one = resolver
            .first_addr()
            .resolve_without_timeout("localhost".into())
            .await
            .unwrap();
        assert_eq!(resolve_one, IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    #[tokio::test]
    async fn parser_resolver() {
        let mut resolver = ParseAddressResolver::new();

        let mut addrs = resolver
            .resolve_without_timeout("127.0.0.1".into())
            .await
            .unwrap();
        assert_eq!(addrs.pop().unwrap().ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));

        let mut addrs = resolver
            .resolve_without_timeout("::1".into())
            .await
            .unwrap();
        assert_eq!(addrs.pop().unwrap().ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));

        let resolve_one = resolver
            .first_addr()
            .resolve_without_timeout("10.0.0.1".into())
            .await
            .unwrap();

        assert_eq!(resolve_one, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
    }
}
