use std::{collections::HashMap, fmt, num::NonZeroUsize, str::FromStr};

use super::UriError;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Token(Option<NonZeroUsize>);

impl Token {
    pub fn zero() -> Self {
        Token(None)
    }

    #[allow(dead_code)]
    pub fn is_zero(&self) -> bool {
        self.0.is_none()
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(value) => write!(f, "Token({value})"),
            None => write!(f, "Token(0)"),
        }
    }
}

pub(crate) struct TokenMap<K> {
    counter: NonZeroUsize,
    map: HashMap<K, Token>,
}

impl<K> fmt::Debug for TokenMap<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenMap")
            .field("counter", &self.counter)
            .finish()
    }
}

impl<K> Default for TokenMap<K> {
    fn default() -> Self {
        Self {
            counter: NonZeroUsize::new(1).unwrap(),
            map: HashMap::new(),
        }
    }
}

impl<K> TokenMap<K>
where
    K: Eq + std::hash::Hash,
{
    pub fn insert(&mut self, key: K) -> Token {
        *self.map.entry(key).or_insert_with(|| {
            let token = Token(Some(self.counter));
            self.counter = self
                .counter
                .checked_add(1)
                .or(NonZeroUsize::new(1))
                .unwrap();
            token
        })
    }
}

/// Pool key which is used to identify a connection - using scheme
/// and authority.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct UriKey(http::uri::Scheme, Option<http::uri::Authority>);

impl fmt::Display for UriKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}://{}",
            self.0,
            self.1.as_ref().map_or("", |a| a.as_str())
        )
    }
}

impl From<(http::uri::Scheme, http::uri::Authority)> for UriKey {
    fn from(value: (http::uri::Scheme, http::uri::Authority)) -> Self {
        Self(value.0, Some(value.1))
    }
}

impl TryFrom<http::Uri> for UriKey {
    type Error = UriError;

    fn try_from(uri: http::Uri) -> Result<Self, Self::Error> {
        let parts = uri.into_parts();
        let authority = parts.authority.clone();
        let scheme = parts
            .scheme
            .clone()
            .ok_or_else(|| UriError::MissingScheme(http::Uri::from_parts(parts).unwrap()))?;
        Ok::<_, UriError>(Self(scheme, authority))
    }
}

impl TryFrom<&http::request::Parts> for UriKey {
    type Error = UriError;

    fn try_from(parts: &http::request::Parts) -> Result<Self, Self::Error> {
        Ok::<_, UriError>(Self(
            parts
                .uri
                .scheme()
                .ok_or_else(|| UriError::MissingScheme(parts.uri.clone()))?
                .clone(),
            parts.uri.authority().cloned(),
        ))
    }
}

impl FromStr for UriKey {
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
        let key: UriKey = uri.try_into().unwrap();
        assert_eq!(key.0, http::uri::Scheme::HTTP);
        assert_eq!(
            key.1,
            Some(http::uri::Authority::from_static("localhost:8080"))
        );
    }

    #[test]
    fn key_display() {
        let key = UriKey(
            http::uri::Scheme::HTTP,
            Some(http::uri::Authority::from_static("localhost:8080")),
        );
        assert_eq!(key.to_string(), "http://localhost:8080");
    }

    #[test]
    fn key_from_tuple() {
        let key: UriKey = (
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
        let key = UriKey(
            http::uri::Scheme::HTTP,
            Some(http::uri::Authority::from_static("localhost:8080")),
        );
        assert_eq!(format!("{key:?}"), "UriKey(\"http\", Some(localhost:8080))");
    }

    #[test]
    fn token_wrap() {
        let mut map = TokenMap {
            counter: NonZeroUsize::new(usize::MAX).unwrap(),
            ..Default::default()
        };
        let foo = map.insert("key");
        assert_eq!(foo.0, Some(NonZeroUsize::new(usize::MAX).unwrap()));

        let bar = map.insert("bar");
        assert_eq!(bar.0, Some(NonZeroUsize::new(1).unwrap()));

        assert_eq!(map.insert("bar"), bar);
        assert_ne!(map.insert("key"), bar);
    }
}
