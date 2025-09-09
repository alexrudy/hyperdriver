use thiserror::Error;

use crate::BoxError;
use chateau::client::conn::ConnectionError;

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

impl<D, T, P, S> From<ConnectionError<D, T, P, S>> for Error
where
    D: Into<BoxError>,
    T: Into<BoxError>,
    P: Into<BoxError>,
    S: Into<BoxError>,
{
    fn from(error: ConnectionError<D, T, P, S>) -> Self {
        match error {
            ConnectionError::Resolving(error) => Error::Transport(error.into()),
            ConnectionError::Connecting(error) => Error::Transport(error.into()),
            ConnectionError::Handshaking(error) => Error::Protocol(error.into()),
            ConnectionError::Service(error) => Error::Connection(error.into()),
            ConnectionError::Unavailable => {
                Error::Connection("pool closed, no connection can be made".into())
            }
            _ => Error::Connection("Unknown error occured".into()),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use static_assertions::assert_impl_all;

    assert_impl_all!(Error: std::error::Error, Send, Sync, Into<BoxError>);
}
