use std::{fmt, str::FromStr};

use thiserror::Error;

/// The URI used for connecting to a server is invalid.
///
/// Usually, this means that the URI is missing a scheme or authority,
/// but it can also mean that the connection string could not be parsed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UriError {
    /// The connection string could not be parsed.
    #[error("invalid uri: {0}")]
    InvalidUri(#[from] http::uri::InvalidUri),

    /// The URI is missing a scheme.
    #[error("missing scheme in uri: {0}")]
    MissingScheme(http::Uri),
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct Key(http::uri::Scheme, Option<http::uri::Authority>);

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}://{}",
            self.0,
            self.1.as_ref().map_or("", |a| a.as_str())
        )
    }
}

impl From<(http::uri::Scheme, http::uri::Authority)> for Key {
    fn from(value: (http::uri::Scheme, http::uri::Authority)) -> Self {
        Self(value.0, Some(value.1))
    }
}

impl TryFrom<http::Uri> for Key {
    type Error = UriError;

    fn try_from(value: http::Uri) -> Result<Self, Self::Error> {
        let parts = value.clone().into_parts();

        Ok::<_, UriError>(Self(
            parts
                .scheme
                .ok_or_else(|| UriError::MissingScheme(value.clone()))?,
            parts.authority,
        ))
    }
}

impl FromStr for Key {
    type Err = UriError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let uri = http::Uri::from_str(s)?;
        uri.try_into()
    }
}

#[cfg(test)]
pub(crate) mod test_key {
    use super::*;

    #[test]
    fn key_from_uri() {
        let uri = http::Uri::from_static("http://localhost:8080");
        let key: Key = uri.try_into().unwrap();
        assert_eq!(key.0, http::uri::Scheme::HTTP);
        assert_eq!(
            key.1,
            Some(http::uri::Authority::from_static("localhost:8080"))
        );
    }

    #[test]
    fn key_display() {
        let key = Key(
            http::uri::Scheme::HTTP,
            Some(http::uri::Authority::from_static("localhost:8080")),
        );
        assert_eq!(key.to_string(), "http://localhost:8080");
    }

    #[test]
    fn key_from_tuple() {
        let key: Key = (
            http::uri::Scheme::HTTP,
            http::uri::Authority::from_static("localhost:8080"),
        )
            .into();
        assert_eq!(key.0, http::uri::Scheme::HTTP);
        assert_eq!(
            key.1,
            Some(http::uri::Authority::from_static("localhost:8080"))
        );
    }

    #[test]
    fn key_debug() {
        let key = Key(
            http::uri::Scheme::HTTP,
            Some(http::uri::Authority::from_static("localhost:8080")),
        );
        assert_eq!(format!("{:?}", key), "Key(\"http\", Some(localhost:8080))");
    }
}
