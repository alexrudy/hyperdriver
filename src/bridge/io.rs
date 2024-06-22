use std::{
    io::Error,
    ops::{Deref, DerefMut},
    pin::Pin,
    task::{Context, Poll},
};

use hyper::rt::Read;
use hyper::rt::Write;

/// Convert Tokio I/O Traits to their Hyper-1.x counterparts
#[derive(Debug)]
#[pin_project::pin_project]
pub struct TokioIo<T> {
    #[pin]
    inner: T,
}

impl<T> TokioIo<T> {
    /// Create a new Tokio I/O wrapper around the given I/O object
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    /// Unwrap the inner I/O object
    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T> Deref for TokioIo<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> DerefMut for TokioIo<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<T> Read for TokioIo<T>
where
    T: tokio::io::AsyncRead,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<Result<(), Error>> {
        #[allow(unsafe_code)]
        let n = unsafe {
            let mut tbuf = tokio::io::ReadBuf::uninit(buf.as_mut());
            match tokio::io::AsyncRead::poll_read(self.project().inner, cx, &mut tbuf) {
                Poll::Ready(Ok(())) => tbuf.filled().len(),
                other => return other,
            }
        };

        #[allow(unsafe_code)]
        unsafe {
            buf.advance(n);
        }
        Poll::Ready(Ok(()))
    }
}

impl<T> Write for TokioIo<T>
where
    T: tokio::io::AsyncWrite,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, Error>> {
        tokio::io::AsyncWrite::poll_write(self.project().inner, cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        tokio::io::AsyncWrite::poll_flush(self.project().inner, cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        tokio::io::AsyncWrite::poll_shutdown(self.project().inner, cx)
    }

    fn is_write_vectored(&self) -> bool {
        tokio::io::AsyncWrite::is_write_vectored(&self.inner)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<Result<usize, Error>> {
        tokio::io::AsyncWrite::poll_write_vectored(self.project().inner, cx, bufs)
    }
}

impl<T> tokio::io::AsyncRead for TokioIo<T>
where
    T: Read,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        tbuf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let filled = tbuf.filled().len();
        #[allow(unsafe_code)]
        let sub_filled = unsafe {
            let mut buf = hyper::rt::ReadBuf::uninit(tbuf.unfilled_mut());

            match Read::poll_read(self.project().inner, cx, buf.unfilled()) {
                Poll::Ready(Ok(())) => buf.filled().len(),
                other => return other,
            }
        };

        let n_filled = filled + sub_filled;
        // At least sub_filled bytes had to have been initialized.
        let n_init = sub_filled;
        #[allow(unsafe_code)]
        unsafe {
            tbuf.assume_init(n_init);
            tbuf.set_filled(n_filled);
        }

        Poll::Ready(Ok(()))
    }
}

impl<T> tokio::io::AsyncWrite for TokioIo<T>
where
    T: Write,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Write::poll_write(self.project().inner, cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Write::poll_flush(self.project().inner, cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Write::poll_shutdown(self.project().inner, cx)
    }

    fn is_write_vectored(&self) -> bool {
        Write::is_write_vectored(&self.inner)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<Result<usize, std::io::Error>> {
        Write::poll_write_vectored(self.project().inner, cx, bufs)
    }
}
