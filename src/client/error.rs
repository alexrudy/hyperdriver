use thiserror::Error;

use crate::BoxError;
use chateau::client::conn::connector::Error as ConnectorError;

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

impl<D, T, P> From<ConnectorError<D, T, P>> for Error
where
    D: Into<BoxError>,
    T: Into<BoxError>,
    P: Into<BoxError>,
{
    fn from(error: ConnectorError<D, T, P>) -> Self {
        match error {
            ConnectorError::Resolving(error) => Error::Connection(error.into()),
            ConnectorError::Connecting(error) => Error::Connection(error.into()),
            ConnectorError::Handshaking(error) => Error::Transport(error.into()),
            ConnectorError::Unavailable => {
                Error::Connection("pool closed, no connection can be made".into())
            }
            _ => Error::Connection("Unknown error occured"),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use static_assertions::assert_impl_all;

    assert_impl_all!(Error: std::error::Error, Send, Sync, Into<BoxError>);
}
