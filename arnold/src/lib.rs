//! Arnold is a Body (http_body::Body) builder

use std::fmt;
use std::pin::pin;
use std::pin::Pin;

#[cfg(feature = "hyper")]
use futures::future::FutureExt;
use http_body::combinators::UnsyncBoxBody;
#[cfg(feature = "hyper")]
use http_body::Body as _;

use bytes::Bytes;

type BoxError = Box<dyn std::error::Error + Sync + std::marker::Send + 'static>;

pub type Request = http::Request<Body>;
pub type Response = http::Response<Body>;

#[derive(Debug)]
#[pin_project::pin_project]
pub struct Body {
    #[pin]
    inner: InnerBody,
}

impl Body {
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

#[cfg(feature = "hyper")]
impl From<hyper::Body> for Body {
    fn from(body: hyper::Body) -> Self {
        Self {
            inner: InnerBody::Hyper(body),
        }
    }
}

#[cfg(feature = "hyper")]
impl TryFrom<Body> for hyper::Body {
    type Error = Body;
    fn try_from(value: Body) -> Result<Self, Self::Error> {
        match value.inner {
            InnerBody::Empty => Ok(hyper::Body::empty()),
            InnerBody::Full(mut full) => {
                if let Some(Some(Ok(data))) = full.data().now_or_never() {
                    Ok(hyper::Body::from(data))
                } else {
                    // One of three things happened here:
                    // 1. The Future returned pending. But we know that can't happen for http_body::Full
                    // 2. The Future returned None - meaning that someone else already polled this future.
                    //    That can only happen while polling this future, which requires Full to be pinned,
                    //    And we know that full is not pinned in this function (except where we poll it).
                    // 3. The Future returned an error. But we know that can't happen for http_body::Full.
                    panic!("Unexpected error while converting Body(Full) to hyper::Body")
                }
            }
            InnerBody::Hyper(body) => Ok(body),
            InnerBody::Boxed(_) => Err(value),
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

impl From<http_body::Full<Bytes>> for Body {
    fn from(body: http_body::Full<Bytes>) -> Self {
        Self {
            inner: InnerBody::Full(body),
        }
    }
}

impl From<http_body::Empty<Bytes>> for Body {
    fn from(_body: http_body::Empty<Bytes>) -> Self {
        Self {
            inner: InnerBody::Empty,
        }
    }
}

impl<E> From<UnsyncBoxBody<Bytes, E>> for Body
where
    E: Into<BoxError> + 'static,
{
    fn from(body: UnsyncBoxBody<Bytes, E>) -> Self {
        Self {
            inner: InnerBody::Boxed(Box::pin(http_body::Body::map_err(body, Into::into))),
        }
    }
}

#[pin_project::pin_project(project = InnerBodyProj)]
enum InnerBody {
    Empty,
    Full(#[pin] http_body::Full<Bytes>),
    #[cfg(feature = "hyper")]
    Hyper(#[pin] hyper::Body),
    Boxed(#[pin] Pin<Box<dyn http_body::Body<Data = Bytes, Error = BoxError> + Send + 'static>>),
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

    fn poll_data(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Self::Data, Self::Error>>> {
        match self.project().inner.project() {
            InnerBodyProj::Empty => std::task::Poll::Ready(None),
            #[cfg(feature = "hyper")]
            InnerBodyProj::Hyper(body) => body
                .poll_data(cx)
                .map(|opt| opt.map(|res| res.map_err(Into::into))),
            InnerBodyProj::Full(body) => body
                .poll_data(cx)
                .map(|opt| opt.map(|res| res.map_err(Into::into))),
            InnerBodyProj::Boxed(body) => body.poll_data(cx),
        }
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        match self.project().inner.project() {
            InnerBodyProj::Empty => std::task::Poll::Ready(Ok(None)),
            #[cfg(feature = "hyper")]
            InnerBodyProj::Hyper(body) => body.poll_trailers(cx).map(|res| res.map_err(Into::into)),
            InnerBodyProj::Full(body) => body.poll_trailers(cx).map(|res| res.map_err(Into::into)),
            InnerBodyProj::Boxed(body) => body.poll_trailers(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self.inner {
            InnerBody::Empty => true,
            #[cfg(feature = "hyper")]
            InnerBody::Hyper(ref body) => body.is_end_stream(),
            InnerBody::Full(ref body) => body.is_end_stream(),
            InnerBody::Boxed(ref body) => body.is_end_stream(),
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match self.inner {
            InnerBody::Empty => http_body::SizeHint::with_exact(0),
            #[cfg(feature = "hyper")]
            InnerBody::Hyper(ref body) => body.size_hint(),
            InnerBody::Full(ref body) => body.size_hint(),
            InnerBody::Boxed(ref body) => body.size_hint(),
        }
    }
}

impl fmt::Debug for InnerBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InnerBody::Empty => f.debug_struct("Empty").finish(),
            InnerBody::Full(_) => f.debug_struct("Full").finish(),
            #[cfg(feature = "hyper")]
            InnerBody::Hyper(_) => f.debug_struct("Hyper").finish(),
            InnerBody::Boxed(_) => f.debug_struct("Boxed").finish(),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use static_assertions::assert_impl_all;

    assert_impl_all!(Body: Send);
}
