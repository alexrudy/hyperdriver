#![allow(unsafe_code)]

use std::mem::MaybeUninit;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::{fmt, future::Future, io};

use http_body::Body;
use hyper::body::Incoming;
use hyper::rt::bounds::Http2ServerConnExec;
use hyper::rt::{ReadBuf, Write};
use hyper::service::HttpService;
use hyper::{body, rt::Read};
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::bridge::rt::TokioExecutor;
use crate::bridge::service::TowerHyperService;
use crate::rewind::Rewind;
use crate::server::Protocol;
use crate::service::IncomingRequestService;
use crate::BoxError;

use super::connecting::Connecting;
use super::{http1, http2, Connection, ConnectionError};

type Adapt<S, BIn, BOut> = TowerHyperService<IncomingRequestService<S, BIn, BOut>>;

const HTTP2_PREFIX: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// The HTTP protocol to use for a connection.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
enum HttpProtocol {
    /// Connect using HTTP/1.1
    Http1,

    /// Connect using HTTP/2
    Http2,
}

/// A builder for creating connections which automatically detect the HTTP protocol version.
///
/// This builder also requires that the server support upgrades from HTTP/1 to HTTP/2.
#[derive(Debug, Clone)]
pub struct Builder<E = TokioExecutor> {
    http1: http1::Builder,
    http2: http2::Builder<E>,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new(TokioExecutor::new())
    }
}

impl<E> Builder<E> {
    /// Create a new `Builder` with the given executor.
    pub fn new(executor: E) -> Self {
        Self {
            http1: http1::Builder::new(),
            http2: http2::Builder::new(executor),
        }
    }

    /// Get a reference to the HTTP/1.1 configuration.
    pub fn http1(&mut self) -> &mut http1::Builder {
        &mut self.http1
    }

    /// Get a reference to the HTTP/2 configuration.
    pub fn http2(&mut self) -> &mut http2::Builder<E> {
        &mut self.http2
    }

    /// Serve a connection with automatic protocol detection.
    pub fn serve_connection_with_upgrades<I, S, B>(
        &self,
        io: I,
        service: S,
    ) -> UpgradableConnection<'_, I, S, E>
    where
        S: hyper::service::HttpService<body::Incoming, ResBody = B> + Clone,
        S::Future: 'static,
        S::Error: Into<BoxError>,
        B: Body + 'static,
        // B::Error: Into<BoxError>,
        I: Read + Write + Unpin + Send + 'static,
    {
        UpgradableConnection {
            state: ConnectionState::ReadVersion {
                read_version: ReadVersion::new(io),
                builder: self,
                service: Some(service),
            },
        }
    }
}

impl<S, IO, BIn, BOut, E> Protocol<S, IO, BIn> for Builder<E>
where
    S: tower::Service<http::Request<BIn>, Response = http::Response<BOut>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    BIn: http_body::Body + From<hyper::body::Incoming> + 'static,
    BOut: http_body::Body + Send + 'static,
    BOut::Data: Send + 'static,
    BOut::Error: Into<BoxError>,
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    E: Http2ServerConnExec<<Adapt<S, BIn, BOut> as HttpService<Incoming>>::Future, BOut>
        + Clone
        + Send
        + Sync
        + 'static,
{
    type ResponseBody = BOut;
    type Connection = Connecting<S, IO, BIn, BOut, E>;
    type Error = ConnectionError;

    fn serve_connection_with_upgrades(&self, stream: IO, service: S) -> Self::Connection {
        Connecting::build(self.clone(), service, stream)
    }
}

/// A combination HTTP/1 and HTTP/2 connection that can upgrade from HTTP/1 to HTTP/2.
#[pin_project]
#[derive(Debug)]
pub struct UpgradableConnection<'b, I, S, E>
where
    S: hyper::service::HttpService<hyper::body::Incoming>,
{
    #[pin]
    state: ConnectionState<'b, I, S, E>,
}

impl<'b, I, S, Executor, B> Connection for UpgradableConnection<'b, I, S, Executor>
where
    S: hyper::service::HttpService<hyper::body::Incoming, ResBody = B> + Clone,
    S::Future: 'static,
    S::Error: Into<BoxError>,
    B: Body + 'static,
    B::Error: Into<BoxError>,
    I: Read + Write + Unpin + Send + 'static,
    Executor: Http2ServerConnExec<S::Future, B>,
{
    fn graceful_shutdown(self: Pin<&mut Self>) {
        let this = self.project();
        match this.state.project() {
            ConnectionStateProject::Http1(conn) => conn.graceful_shutdown(),
            ConnectionStateProject::Http2(conn) => conn.graceful_shutdown(),
            ConnectionStateProject::ReadVersion { read_version, .. } => {
                read_version.cancel();
            }
        }
    }
}

impl<'b, I, S, E, B> Future for UpgradableConnection<'b, I, S, E>
where
    S: hyper::service::HttpService<hyper::body::Incoming, ResBody = B> + Clone,
    S::Future: 'static,
    S::Error: Into<BoxError>,
    B: Body + 'static,
    B::Error: Into<BoxError>,
    I: Read + Write + Unpin + Send + 'static,
    E: Http2ServerConnExec<S::Future, B>,
{
    type Output = Result<(), ConnectionError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            let mut this = self.as_mut().project();

            match this.state.as_mut().project() {
                ConnectionStateProject::ReadVersion {
                    read_version,
                    builder,
                    service,
                } => {
                    let (version, rewind) =
                        ready!(read_version.poll(cx)).map_err(ConnectionError::Protocol)?;
                    let service = service.take().unwrap();
                    let conn = match version {
                        HttpProtocol::Http1 => ConnectionState::Http1(
                            builder
                                .http1
                                .serve_connection(rewind, service)
                                .with_upgrades(),
                        ),
                        HttpProtocol::Http2 => {
                            ConnectionState::Http2(builder.http2.serve_connection(rewind, service))
                        }
                    };
                    this.state.set(conn);
                }
                ConnectionStateProject::Http1(conn) => {
                    return conn.poll(cx).map_err(Into::into);
                }
                ConnectionStateProject::Http2(conn) => {
                    return conn.poll(cx).map_err(Into::into);
                }
            }
        }
    }
}

#[pin_project(project = ConnectionStateProject)]
enum ConnectionState<'b, I, S, E>
where
    S: hyper::service::HttpService<hyper::body::Incoming>,
{
    ReadVersion {
        #[pin]
        read_version: ReadVersion<I>,
        builder: &'b Builder<E>,
        service: Option<S>,
    },
    Http1(#[pin] http1::UpgradeableConnection<Rewind<I>, S>),
    Http2(#[pin] http2::Connection<Rewind<I>, S, E>),
}

impl<'b, I, S, E> fmt::Debug for ConnectionState<'b, I, S, E>
where
    S: hyper::service::HttpService<body::Incoming>,
    I: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectionState::ReadVersion { read_version, .. } => f
                .debug_struct("ReadVersion")
                .field("read_version", &read_version)
                .finish(),
            ConnectionState::Http1(_) => f.debug_struct("ConnectionState::Http1").finish(),
            ConnectionState::Http2(conn) => f
                .debug_struct("ConnectionState::Http2")
                .field("connection", conn)
                .finish(),
        }
    }
}

#[derive(Debug)]
#[pin_project]
#[must_use = "futures do nothing unless you `.await` or poll them"]
struct ReadVersion<I> {
    io: Option<I>,
    buf: [MaybeUninit<u8>; 24],
    filled: usize,
    version: HttpProtocol,
    cancelled: bool,
}

impl<I> ReadVersion<I> {
    fn cancel(self: Pin<&mut Self>) {
        *self.project().cancelled = true;
    }

    fn new(io: I) -> Self {
        ReadVersion {
            io: Some(io),
            buf: [MaybeUninit::uninit(); 24],
            filled: 0,
            version: HttpProtocol::Http2,
            cancelled: false,
        }
    }
}

impl<I> Future for ReadVersion<I>
where
    I: Read + Unpin,
{
    type Output = Result<(HttpProtocol, Rewind<I>), io::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        if *this.cancelled {
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::Interrupted)));
        }

        let mut buf = ReadBuf::uninit(this.buf);

        unsafe {
            buf.unfilled().advance(*this.filled);
        }

        while buf.filled().len() < HTTP2_PREFIX.len() {
            let len = buf.filled().len();
            ready!(Pin::new(this.io.as_mut().unwrap()).poll_read(cx, buf.unfilled()))?;
            *this.filled = buf.filled().len();

            if buf.filled().len() == len || buf.filled()[len..] != HTTP2_PREFIX[len..] {
                *this.version = HttpProtocol::Http1;
                break;
            }
        }

        let io = this.io.take().unwrap();
        let rewind = Rewind::new(io, buf.filled().to_vec());
        Poll::Ready(Ok((*this.version, rewind)))
    }
}

#[cfg(test)]
mod tests {

    use tokio::io::{AsyncReadExt, AsyncWriteExt as _};

    use crate::bridge::io::TokioIo;

    use super::*;

    #[tokio::test]
    async fn test_read_version_h2() {
        let (io, mut srv) = tokio::io::duplex(1024);

        srv.write_all(HTTP2_PREFIX).await.unwrap();
        srv.flush().await.unwrap();

        let read_version = ReadVersion::new(TokioIo::new(io));

        let (version, rewind) = read_version.await.unwrap();
        assert_eq!(version, HttpProtocol::Http2);

        let (mut io, prefix) = rewind.into_parts();
        assert_eq!(prefix.as_deref(), Some(HTTP2_PREFIX));

        let mut buf = Vec::new();
        tokio::try_join!(srv.shutdown(), io.read_to_end(&mut buf)).unwrap();
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn test_read_version_h1() {
        let (io, mut srv) = tokio::io::duplex(1024);

        srv.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        srv.flush().await.unwrap();

        let read_version = ReadVersion::new(TokioIo::new(io));

        let (version, rewind) = read_version.await.unwrap();
        assert_eq!(version, HttpProtocol::Http1);

        let (mut io, prefix) = rewind.into_parts();
        assert_eq!(
            prefix.as_deref(),
            Some(b"GET / HTTP/1.1\r\n\r\n".as_slice()),
            "prefix"
        );

        let mut buf = Vec::new();
        tokio::try_join!(srv.shutdown(), io.read_to_end(&mut buf)).unwrap();
        assert!(buf.is_empty(), "buffer");
    }

    #[tokio::test]
    async fn test_rewind_returns_full_data() {
        let (io, mut srv) = tokio::io::duplex(1024);
        srv.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        srv.flush().await.unwrap();

        let read_version = ReadVersion::new(TokioIo::new(io));

        let (version, rewind) = read_version.await.unwrap();
        assert_eq!(version, HttpProtocol::Http1);

        let mut buf = Vec::new();
        let mut io = TokioIo::new(rewind);
        tokio::try_join!(srv.shutdown(), io.read_to_end(&mut buf)).unwrap();
        assert_eq!(b"GET / HTTP/1.1\r\n\r\n", buf.as_slice());
    }
}
