use ::http::Uri;
use braid::client::Stream;

use tower::Service;

mod builder;
mod dns;
mod http;
mod tcp;

pub use self::builder::ConnectionError;
pub(crate) use self::builder::{Builder, Connection};
pub(crate) use self::http::HttpConnector;
pub(crate) use self::tcp::TcpConnectionConfig;
use self::tcp::TcpConnectionError;
pub(crate) use self::tcp::TcpConnector;

/// Trait for types which implement a [tower::Service] appropriate for opening a
/// [Stream] to a [Uri].
pub trait Transport
where
    Self: Service<Uri, Response = Stream, Error = TcpConnectionError>,
{
}

impl<T> Transport for T where T: Service<Uri, Response = Stream, Error = TcpConnectionError> {}

/// Trait for types which implement a [tower::Service] appropriate for connecting to a
/// [Uri].
pub trait Connect
where
    Self: Service<Uri, Response = Connection, Error = ConnectionError>,
{
}

impl<T> Connect for T where T: Service<Uri, Response = Connection, Error = ConnectionError> {}

/// The HTTP protocol to use for a connection.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum ConnectionProtocol {
    /// Connect using HTTP/1.1
    Http1,

    /// Connect using HTTP/2
    #[allow(dead_code)]
    Http2,
}

impl ConnectionProtocol {
    #[allow(dead_code)]
    /// Does this protocol allow multiplexing?
    pub fn multiplex(&self) -> bool {
        matches!(self, Self::Http2)
    }
}
