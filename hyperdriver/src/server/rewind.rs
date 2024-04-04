//! Module to rewind a readable buffer after prefix hunting
//!

#![allow(unsafe_code)]

use std::{pin::Pin, task::Poll};

use bytes::{Buf, Bytes};
use hyper::rt::{Read, ReadBufCursor, Write};

pub struct Rewind<R> {
    inner: R,
    prefix: Option<Bytes>,
}

impl<R> Rewind<R> {
    pub fn new<B>(inner: R, prefix: B) -> Self
    where
        B: Into<Bytes>,
    {
        Self {
            inner,
            prefix: Some(prefix.into()),
        }
    }
}

impl<T> Read for Rewind<T>
where
    T: Read + Unpin,
{
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context,
        mut buf: ReadBufCursor<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if let Some(mut prefix) = self.prefix.take() {
            if !prefix.is_empty() {
                let n = std::cmp::min(prefix.len(), remaining(&mut buf));

                put_slice(&mut buf, &prefix[..n]);
                prefix.advance(n);

                if !prefix.is_empty() {
                    self.prefix = Some(prefix);
                }
                return Poll::Ready(Ok(()));
            }
        }

        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T> Write for Rewind<T>
where
    T: Write + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

// These two functions expose pub(crate) functions from `ReadBufCursor`.
fn remaining(cursor: &mut ReadBufCursor<'_>) -> usize {
    // SAFETY:
    // We do not uninitialize any set bytes.
    unsafe { cursor.as_mut().len() }
}

fn put_slice(cursor: &mut ReadBufCursor<'_>, slice: &[u8]) {
    assert!(
        remaining(cursor) >= slice.len(),
        "buf.len() must fit in remaining()"
    );

    let amt = slice.len();

    // SAFETY:
    // the length is asserted above
    unsafe {
        cursor.as_mut()[..amt]
            .as_mut_ptr()
            .cast::<u8>()
            .copy_from_nonoverlapping(slice.as_ptr(), amt);
        cursor.advance(amt);
    }
}
