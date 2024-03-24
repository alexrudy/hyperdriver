use std::fmt;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct Key(http::uri::Scheme, http::uri::Authority);

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}", self.0, self.1)
    }
}

impl From<(http::uri::Scheme, http::uri::Authority)> for Key {
    fn from(value: (http::uri::Scheme, http::uri::Authority)) -> Self {
        Self(value.0, value.1)
    }
}

impl From<http::Uri> for Key {
    fn from(value: http::Uri) -> Self {
        let parts = value.into_parts();

        Self(parts.scheme.unwrap(), parts.authority.unwrap())
    }
}

#[cfg(test)]
pub(crate) mod test_key {
    use super::*;

    #[test]
    fn key_from_uri() {
        let uri = http::Uri::from_static("http://localhost:8080");
        let key: Key = uri.into();
        assert_eq!(key.0, http::uri::Scheme::HTTP);
        assert_eq!(key.1, http::uri::Authority::from_static("localhost:8080"));
    }

    #[test]
    fn key_display() {
        let key = Key(
            http::uri::Scheme::HTTP,
            http::uri::Authority::from_static("localhost:8080"),
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
        assert_eq!(key.1, http::uri::Authority::from_static("localhost:8080"));
    }

    #[test]
    fn key_debug() {
        let key = Key(
            http::uri::Scheme::HTTP,
            http::uri::Authority::from_static("localhost:8080"),
        );
        assert_eq!(format!("{:?}", key), "Key(\"http\", localhost:8080)");
    }
}
