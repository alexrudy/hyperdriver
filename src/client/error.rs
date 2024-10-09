use thiserror::Error;

use super::pool;
use crate::BoxError;

/// Client error type.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Error occured with the underlying connection.
    #[error("connection: {0}")]
    Connection(#[source] BoxError),

    /// Error occured with the underlying transport.
    #[error("transport: {0}")]
    Transport(#[source] BoxError),

    /// Error occured with the underlying protocol.
    #[error("protocol: {0}")]
    Protocol(#[source] BoxError),

    /// Error occured with the underlying service
    #[error("serivce: {0}")]
    Service(#[source] BoxError),

    /// Error occured with the user's request, such as an invalid URI.
    #[error("user error: {0}")]
    User(#[source] hyper::Error),

    /// Invalid HTTP Method for the current action.
    #[error("invalid method: {0}")]
    InvalidMethod(http::Method),

    /// Protocol is not supported by this client or transport.
    #[error("unsupported protocol")]
    UnsupportedProtocol,

    /// Request timeout
    #[error("request timeout")]
    RequestTimeout,
}

impl<E1, E2> From<pool::Error<E1, E2>> for Error
where
    E1: Into<BoxError>,
    E2: Into<BoxError>,
{
    fn from(error: pool::Error<E1, E2>) -> Self {
        match error {
            pool::Error::Connecting(error) => Error::Connection(error.into()),
            pool::Error::Handshaking(error) => Error::Transport(error.into()),
            pool::Error::Unavailable => {
                Error::Connection("pool closed, no connection can be made".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use static_assertions::assert_impl_all;

    assert_impl_all!(Error: std::error::Error, Send, Sync, Into<BoxError>);
}
