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

pub trait Transport
where
    Self: Service<Uri, Response = Stream, Error = TcpConnectionError>,
{
}

impl<T> Transport for T where T: Service<Uri, Response = Stream, Error = TcpConnectionError> {}

pub trait Connect
where
    Self: Service<Uri, Response = Connection, Error = ConnectionError>,
{
}

impl<T> Connect for T where T: Service<Uri, Response = Connection, Error = ConnectionError> {}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum ConnectionProtocol {
    Http1,

    #[allow(dead_code)]
    Http2,
}

impl ConnectionProtocol {
    #[allow(dead_code)]
    pub fn multiplex(&self) -> bool {
        matches!(self, Self::Http2)
    }
}
