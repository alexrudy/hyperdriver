//! Arnold is a [Body](http_body::Body) builder, useful for wrapping
//! different potential bodies for sending responses or requests.
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
use http_body::Body as _;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::BodyExt;
use http_body_util::{Empty, Full};

#[cfg(feature = "incoming")]
pub use crate::service::{AdaptIncomingLayer, AdaptIncomingService};
type BoxError = Box<dyn std::error::Error + Sync + std::marker::Send + 'static>;

/// An http request using [Body] as the body.
pub type Request = http::Request<Body>;

/// An http response using [Body] as the body.
pub type Response = http::Response<Body>;

/// A wrapper for different internal body types which implements [http_body::Body](http_body::Body)
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

            #[cfg(feature = "axum")]
            InnerBody::AxumBody(_) => None,
        }
    }

    /// Convert this body into a boxed body.
    pub fn as_boxed(self) -> UnsyncBoxBody<Bytes, BoxError> {
        match self.inner {
            InnerBody::Full(body) => UnsyncBoxBody::new(body.map_err(|_| unreachable!())),
            InnerBody::Empty => {
                UnsyncBoxBody::new(http_body_util::Empty::new().map_err(|_| unreachable!()))
            }

            #[cfg(feature = "incoming")]
            InnerBody::Incoming(incoming) => UnsyncBoxBody::new(incoming.map_err(Into::into)),

            #[cfg(feature = "axum")]
            InnerBody::AxumBody(body) => UnsyncBoxBody::new(body.map_err(Into::into)),
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

#[cfg(feature = "incoming")]
impl From<hyper::body::Incoming> for Body {
    fn from(body: hyper::body::Incoming) -> Self {
        Self {
            inner: InnerBody::Incoming(body),
        }
    }
}

#[cfg(feature = "axum")]
impl From<axum::body::Body> for Body {
    fn from(body: axum::body::Body) -> Self {
        Self {
            inner: InnerBody::AxumBody(body),
        }
    }
}

#[cfg(feature = "axum")]
impl From<Body> for axum::body::Body {
    fn from(body: Body) -> Self {
        axum::body::Body::new(body)
    }
}

#[pin_project::pin_project(project = InnerBodyProj)]
enum InnerBody {
    Empty,
    Full(#[pin] Full<Bytes>),

    #[cfg(feature = "incoming")]
    Incoming(#[pin] hyper::body::Incoming),

    #[cfg(feature = "axum")]
    AxumBody(#[pin] axum::body::Body),
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

            #[cfg(feature = "axum")]
            InnerBodyProj::AxumBody(body) => poll_frame!(body, cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self.inner {
            InnerBody::Empty => true,
            InnerBody::Full(ref body) => body.is_end_stream(),

            #[cfg(feature = "incoming")]
            InnerBody::Incoming(ref body) => body.is_end_stream(),
            #[cfg(feature = "axum")]
            InnerBody::AxumBody(ref body) => body.is_end_stream(),
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match self.inner {
            InnerBody::Empty => http_body::SizeHint::with_exact(0),
            InnerBody::Full(ref body) => body.size_hint(),

            #[cfg(feature = "incoming")]
            InnerBody::Incoming(ref body) => body.size_hint(),
            #[cfg(feature = "axum")]
            InnerBody::AxumBody(ref body) => body.size_hint(),
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
            #[cfg(feature = "axum")]
            InnerBody::AxumBody(_) => f.debug_struct("AxumBody").finish(),
        }
    }
}

/// Extension trait to help clone a request that contains a `Body`.
pub trait TryCloneRequest {
    /// Try to clone the request. If the body can't be cloned, `None` is returned.
    fn try_clone_request(&self) -> Option<Self>
    where
        Self: Sized;
}

impl TryCloneRequest for http::Request<Body> {
    fn try_clone_request(&self) -> Option<Self> {
        if let Some(body) = self.body().try_clone() {
            let mut req = http::Request::builder()
                .uri(self.uri().clone())
                .method(self.method().clone())
                .version(self.version());

            if let Some(headers) = req.headers_mut() {
                *headers = self.headers().clone();
            };

            Some(req.body(body).expect("request builder should succeed"))
        } else if self.body().size_hint().exact() == Some(0) {
            let mut req = http::Request::builder()
                .uri(self.uri().clone())
                .method(self.method().clone())
                .version(self.version());

            if let Some(headers) = req.headers_mut() {
                *headers = self.headers().clone();
            };

            Some(
                req.body(Body::empty())
                    .expect("request builder should succeed"),
            )
        } else {
            None
        }
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
