//! Arnold is a [Body](http_body::Body) builder, useful for wrapping
//! different potential bodies for sending responses or requests.
#![cfg_attr(
    feature = "docs",
    doc = r#"
For reading incoming requests, defer to [hyper::body::Incoming].
"#
)]
#![deny(missing_docs)]
#![deny(unsafe_code)]

use std::fmt;
use std::pin::pin;
use std::pin::Pin;

use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::BodyExt;
use http_body_util::{Empty, Full};

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

impl<E> From<UnsyncBoxBody<Bytes, E>> for Body
where
    E: Into<BoxError> + 'static,
{
    fn from(body: UnsyncBoxBody<Bytes, E>) -> Self {
        Self {
            inner: InnerBody::Boxed(Box::pin(body.map_err(Into::into))),
        }
    }
}

#[pin_project::pin_project(project = InnerBodyProj)]
enum InnerBody {
    Empty,
    Full(#[pin] Full<Bytes>),
    Boxed(#[pin] Pin<Box<dyn http_body::Body<Data = Bytes, Error = BoxError> + Send + 'static>>),

    #[cfg(feature = "incoming")]
    Incoming(#[pin] hyper::body::Incoming),
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
            InnerBodyProj::Full(body) => body
                .poll_frame(cx)
                .map(|opt| opt.map(|res| res.map_err(Into::into))),
            InnerBodyProj::Boxed(body) => body
                .poll_frame(cx)
                .map(|opt| opt.map(|res| res.map_err(Into::into))),
            #[cfg(feature = "incoming")]
            InnerBodyProj::Incoming(body) => body
                .poll_frame(cx)
                .map(|opt| opt.map(|res| res.map_err(Into::into))),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self.inner {
            InnerBody::Empty => true,
            InnerBody::Full(ref body) => body.is_end_stream(),
            InnerBody::Boxed(ref body) => body.is_end_stream(),
            #[cfg(feature = "incoming")]
            InnerBody::Incoming(ref body) => body.is_end_stream(),
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match self.inner {
            InnerBody::Empty => http_body::SizeHint::with_exact(0),
            InnerBody::Full(ref body) => body.size_hint(),
            InnerBody::Boxed(ref body) => body.size_hint(),
            #[cfg(feature = "incoming")]
            InnerBody::Incoming(ref body) => body.size_hint(),
        }
    }
}

impl fmt::Debug for InnerBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InnerBody::Empty => f.debug_struct("Empty").finish(),
            InnerBody::Full(_) => f.debug_struct("Full").finish(),
            InnerBody::Boxed(_) => f.debug_struct("Boxed").finish(),
            #[cfg(feature = "incoming")]
            InnerBody::Incoming(_) => f.debug_struct("Incoming").finish(),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use static_assertions::assert_impl_all;

    assert_impl_all!(Body: Send);
}
