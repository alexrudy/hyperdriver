use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::{fmt, io};

use futures_util::Future;
use pin_project::{pin_project, pinned_drop};
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Default)]
pub struct SocketAddrs(Vec<SocketAddr>);

impl SocketAddrs {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn push(&mut self, addr: SocketAddr) {
        self.0.push(addr)
    }

    pub(crate) fn set_port(&mut self, port: u16) {
        for addr in &mut self.0 {
            addr.set_port(port)
        }
    }

    pub(crate) fn split_prefered(self, prefer: Option<IpVersion>) -> (Self, Self) {
        if let Some(version) = prefer {
            let mut v4 = Self::new();
            let mut v6 = Self::new();
            for addr in self.0 {
                match addr {
                    SocketAddr::V4(_) => v4.push(addr),
                    SocketAddr::V6(_) => v6.push(addr),
                }
            }
            match version {
                IpVersion::V4 => (v4, v6),
                IpVersion::V6 => (v6, v4),
            }
        } else {
            (self, Self::new())
        }
    }
}

impl Deref for SocketAddrs {
    type Target = [SocketAddr];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for SocketAddrs {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl FromIterator<SocketAddr> for SocketAddrs {
    fn from_iter<T: IntoIterator<Item = SocketAddr>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl<'a> IntoIterator for &'a SocketAddrs {
    type Item = &'a SocketAddr;
    type IntoIter = std::slice::Iter<'a, SocketAddr>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

pub trait IpVersionExt {
    fn version(&self) -> IpVersion;
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum IpVersion {
    V4,
    V6,
}

impl IpVersion {
    pub(super) fn from_binding(
        ip_v4_address: Option<Ipv4Addr>,
        ip_v6_address: Option<Ipv6Addr>,
    ) -> Option<Self> {
        match (ip_v4_address, ip_v6_address) {
            (Some(_), Some(_)) => None,
            (Some(_), None) => Some(Self::V4),
            (None, Some(_)) => Some(Self::V6),
            (None, None) => None,
        }
    }

    #[allow(dead_code)]
    pub fn is_v4(&self) -> bool {
        matches!(self, Self::V4)
    }

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

#[derive(Debug, Default, Clone, Copy)]
pub struct GaiResolver {
    _priv: (),
}

impl GaiResolver {
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
        use std::net::ToSocketAddrs;
        GaiFuture {
            handle: tokio::task::spawn_blocking(move || {
                (host.as_ref(), 0)
                    .to_socket_addrs()
                    .map(SocketAddrs::from_iter)
            }),
        }
    }
}

/// Future returned by `GaiResolver` when resolving
/// via getaddrinfo.
#[pin_project(PinnedDrop)]
pub struct GaiFuture {
    #[pin]
    handle: JoinHandle<Result<SocketAddrs, io::Error>>,
}

impl fmt::Debug for GaiFuture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GaiFuture").finish()
    }
}

impl Future for GaiFuture {
    type Output = Result<SocketAddrs, io::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match ready!(self.project().handle.poll(cx)) {
            Ok(Ok(addrs)) => Poll::Ready(Ok(addrs)),
            Ok(Err(error)) => Poll::Ready(Err(error)),
            Err(join_err) => {
                if join_err.is_cancelled() {
                    Poll::Ready(Err(io::Error::new(io::ErrorKind::Interrupted, join_err)))
                } else {
                    Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, join_err)))
                }
            }
        }
    }
}

#[pinned_drop]
impl PinnedDrop for GaiFuture {
    fn drop(self: Pin<&mut Self>) {
        self.handle.abort()
    }
}
