//! A wrapper [Body](http_body::Body) type with limited support
//! for in-memory bodies and [hyper::body::Incoming]. Optionally
//! includes support for [axum][] bodies.
//!
//! # Examples
//!
//! A body can be created empty (for GET or HEAD requests), or from a [`String`]
//! or [`Bytes`]. The two requests below will have identical types, but the
//! empty request will not internally store a string, effectively making it
//! the size of a pointer.
//!
//! ```rust
//! # use hyperdriver::Body;
//! let mut req: http::Request<Body> = http::Request::get("http://example.com").body(Body::empty()).unwrap();
//! // Note that they are the same type:
//! req = http::Request::post("http://example.com").body(Body::from("hello")).unwrap();
//!```
#![cfg_attr(
    feature = "docs",
    doc = r#"
For reading incoming requests, defer to [hyper::body::Incoming].
"#
)]

use std::fmt;
use std::pin::pin;
use std::pin::Pin;

use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::BodyExt;
use http_body_util::{Empty, Full};

#[cfg(feature = "incoming")]
pub use crate::service::{
    IncomingRequestLayer, IncomingRequestService, IncomingResponseLayer, IncomingResponseService,
};
pub mod adapt;

type BoxError = Box<dyn std::error::Error + Sync + std::marker::Send + 'static>;
type BoxBody = UnsyncBoxBody<Bytes, BodyError>;

/// A wrapper for different internal body types which implements [http_body::Body](http_body::Body)
/// It is always backed by [`Bytes`] for simplicity.
///
/// Bodies can be created from [`Bytes`](bytes::Bytes), [`String`](std::string::String),
/// or [`&'static str`](str) using [`From`](std::convert::From) implementations.
///
/// An empty body can be created with [Body::empty](Body::empty).
#[derive(Debug)]
#[pin_project::pin_project]
pub struct Body {
    #[pin]
    inner: InnerBody,
}

impl Body {
    /// Create a new empty body.
    pub fn empty() -> Self {
        Self {
            inner: InnerBody::Empty,
        }
    }

    /// Create a new body from something which can be converted into [`Bytes`].
    pub fn full<D>(data: D) -> Self
    where
        D: Into<Bytes>,
    {
        Self {
            inner: InnerBody::Full(Full::new(data.into())),
        }
    }

    /// Create a new body from another body type. This method tries to be as unconstrained as possible,
    /// and internally uses downcasting to avoid just making another boxed body indirection.
    pub fn new<B>(body: B) -> Self
    where
        B: http_body::Body<Data = Bytes> + Send + 'static,
        <B as http_body::Body>::Error: Into<BodyError> + 'static,
    {
        if body.is_end_stream() {
            return Self::empty();
        }

        let mut body = Some(body);

        if let Some(body) = <dyn std::any::Any>::downcast_mut::<Option<Body>>(&mut body) {
            return body.take().unwrap();
        }

        #[cfg(feature = "incoming")]
        if let Some(body) =
            <dyn std::any::Any>::downcast_mut::<Option<hyper::body::Incoming>>(&mut body)
        {
            return Self {
                inner: InnerBody::Incoming(body.take().unwrap()),
            };
        }

        if let Some(body) = <dyn std::any::Any>::downcast_mut::<Option<BoxBody>>(&mut body) {
            Self {
                inner: InnerBody::Boxed(body.take().unwrap()),
            }
        } else {
            Self {
                inner: InnerBody::Boxed(body.take().unwrap().map_err(Into::into).boxed_unsync()),
            }
        }
    }

    /// Try to clone this body.
    pub fn try_clone(&self) -> Option<Self> {
        match &self.inner {
            InnerBody::Full(body) => Some(Self {
                inner: InnerBody::Full(body.clone()),
            }),
            InnerBody::Empty => Some(Self {
                inner: InnerBody::Empty,
            }),

            #[cfg(feature = "incoming")]
            InnerBody::Incoming(_) => None,

            InnerBody::Boxed(_) => None,
        }
    }

    /// Convert this body into a boxed body.
    ///
    /// # Example
    /// ```rust
    /// # use hyperdriver::Body;
    /// # use bytes::Bytes;
    /// # use std::error::Error;
    /// # use http_body_util::combinators::UnsyncBoxBody;
    /// let req: http::Request<Body> = http::Request::post("http://example.com").body(Body::from("hello")).unwrap();
    /// let req: http::Request<UnsyncBoxBody<Bytes, Box<dyn Error + Send + Sync>>> = req.map(|body| body.as_boxed());
    /// ```
    pub fn as_boxed(self) -> UnsyncBoxBody<Bytes, BoxError> {
        match self.inner {
            InnerBody::Full(body) => UnsyncBoxBody::new(body.map_err(|_| unreachable!())),
            InnerBody::Empty => {
                UnsyncBoxBody::new(http_body_util::Empty::new().map_err(|_| unreachable!()))
            }

            #[cfg(feature = "incoming")]
            InnerBody::Incoming(incoming) => UnsyncBoxBody::new(incoming.map_err(Into::into)),

            InnerBody::Boxed(boxed) => boxed.map_err(Into::into).boxed_unsync(),
        }
    }
}

impl Default for Body {
    fn default() -> Self {
        Self {
            inner: InnerBody::Empty,
        }
    }
}

impl From<Bytes> for Body {
    fn from(body: Bytes) -> Self {
        Self {
            inner: InnerBody::Full(body.into()),
        }
    }
}

impl From<String> for Body {
    fn from(body: String) -> Self {
        Self { inner: body.into() }
    }
}

impl From<&'static str> for Body {
    fn from(body: &'static str) -> Self {
        Self {
            inner: InnerBody::Full(body.into()),
        }
    }
}

impl From<Vec<u8>> for Body {
    fn from(body: Vec<u8>) -> Self {
        Self {
            inner: InnerBody::Full(body.into()),
        }
    }
}

impl From<Full<Bytes>> for Body {
    fn from(body: Full<Bytes>) -> Self {
        Self {
            inner: InnerBody::Full(body),
        }
    }
}

impl From<Empty<Bytes>> for Body {
    fn from(_body: Empty<Bytes>) -> Self {
        Self {
            inner: InnerBody::Empty,
        }
    }
}

impl From<UnsyncBoxBody<Bytes, BoxError>> for Body {
    fn from(value: UnsyncBoxBody<Bytes, BoxError>) -> Self {
        Self {
            inner: InnerBody::Boxed(value.map_err(Into::into).boxed_unsync()),
        }
    }
}

#[cfg(feature = "incoming")]
impl From<hyper::body::Incoming> for Body {
    fn from(body: hyper::body::Incoming) -> Self {
        Self {
            inner: InnerBody::Incoming(body),
        }
    }
}

#[pin_project::pin_project(project = InnerBodyProj)]
enum InnerBody {
    Empty,
    Full(#[pin] Full<Bytes>),

    #[cfg(feature = "incoming")]
    Incoming(#[pin] hyper::body::Incoming),

    Boxed(UnsyncBoxBody<Bytes, BodyError>),
}

impl From<String> for InnerBody {
    fn from(body: String) -> Self {
        if body.is_empty() {
            Self::Empty
        } else {
            Self::Full(body.into())
        }
    }
}

macro_rules! poll_frame {
    ($body:ident, $cx:ident) => {
        $body
            .poll_frame($cx)
            .map(|opt| opt.map(|res| res.map_err(Into::into)))
    };
}

impl http_body::Body for Body {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = self.project();
        match this.inner.project() {
            InnerBodyProj::Empty => std::task::Poll::Ready(None),
            InnerBodyProj::Full(body) => poll_frame!(body, cx),

            #[cfg(feature = "incoming")]
            InnerBodyProj::Incoming(body) => poll_frame!(body, cx),

            InnerBodyProj::Boxed(body) => {
                let body = Pin::new(body);
                poll_frame!(body, cx)
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        match self.inner {
            InnerBody::Empty => true,
            InnerBody::Full(ref body) => body.is_end_stream(),

            #[cfg(feature = "incoming")]
            InnerBody::Incoming(ref body) => body.is_end_stream(),

            InnerBody::Boxed(ref body) => body.is_end_stream(),
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match self.inner {
            InnerBody::Empty => http_body::SizeHint::with_exact(0),
            InnerBody::Full(ref body) => body.size_hint(),

            #[cfg(feature = "incoming")]
            InnerBody::Incoming(ref body) => body.size_hint(),

            InnerBody::Boxed(ref body) => body.size_hint(),
        }
    }
}

impl fmt::Debug for InnerBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InnerBody::Empty => f.debug_struct("Empty").finish(),
            InnerBody::Full(_) => f.debug_struct("Full").finish(),

            #[cfg(feature = "incoming")]
            InnerBody::Incoming(_) => f.debug_struct("Incoming").finish(),

            InnerBody::Boxed(_) => f.debug_struct("Boxed").finish(),
        }
    }
}

/// An error type which wraps Box<dyn Error + Send + Sync> so that itself implements
/// the Error trait.
pub struct BodyError {
    inner: BoxError,
}

impl BodyError {
    /// Creates a new `BodyError` from the given error.
    pub fn new<E>(error: E) -> Self
    where
        E: std::error::Error + Sync + std::marker::Send + 'static,
    {
        let mut error = Some(error);
        if let Some(error) = <dyn std::any::Any>::downcast_mut::<Option<BoxError>>(&mut error) {
            Self {
                inner: error.take().unwrap(),
            }
        } else {
            Self {
                inner: Box::new(error.take().unwrap()),
            }
        }
    }
}

impl fmt::Debug for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

impl std::error::Error for BodyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source()
    }
}

impl From<BoxError> for BodyError {
    fn from(inner: BoxError) -> Self {
        Self { inner }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use http_body::Body as HttpBody;
    use static_assertions::assert_impl_all;

    assert_impl_all!(Body: HttpBody, Send);

    #[test]
    fn check_body_from_string() {
        let body = Body::from("Hello, World!".to_string());
        assert_eq!(body.size_hint().lower(), 13);
        assert_eq!(body.size_hint().upper(), Some(13));
        assert!(!body.is_end_stream());
    }

    #[test]
    fn check_body_from_empty_string() {
        let body = Body::from("".to_string());
        assert_eq!(body.size_hint().lower(), 0);
        assert_eq!(body.size_hint().upper(), Some(0));
        assert!(body.is_end_stream());
    }

    #[test]
    fn check_body_from_vec() {
        let body = Body::from("Hello, World!".to_string().into_bytes());
        assert_eq!(body.size_hint().lower(), 13);
        assert_eq!(body.size_hint().upper(), Some(13));
        assert!(!body.is_end_stream());
    }

    #[test]
    fn check_body_empty() {
        let body = Body::empty();
        assert_eq!(body.size_hint().lower(), 0);
        assert_eq!(body.size_hint().upper(), Some(0));
        assert!(body.is_end_stream());
    }

    #[test]
    fn check_body_from_bytes() {
        let body = Body::from(Bytes::from("Hello, World!"));
        assert_eq!(body.size_hint().lower(), 13);
        assert_eq!(body.size_hint().upper(), Some(13));
        assert!(!body.is_end_stream());
    }

    #[test]
    fn check_body_from_empty_bytes() {
        let body = Body::from(Bytes::new());
        assert_eq!(body.size_hint().lower(), 0);
        assert_eq!(body.size_hint().upper(), Some(0));
        assert!(body.is_end_stream());
    }
}
