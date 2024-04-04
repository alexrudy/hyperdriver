//! A server framework to interoperate with hyper-v1, tokio, braid and arnold

#![warn(missing_docs)]
#![warn(missing_debug_implementations)]
#![deny(unsafe_code)]

use std::future::{Future, IntoFuture};
use std::pin::{pin, Pin};
use std::sync::Arc;
use std::task::{ready, Context, Poll};
use std::{fmt, io};

pub use self::conn::auto::Builder as AutoBuilder;
use self::conn::Connection;
use crate::bridge::rt::TokioExecutor;
use crate::stream::info::Connection as HasConnectionInfo;
use crate::stream::server::Accept;
use futures_util::future::FutureExt as _;
use tower::make::MakeService;
use tracing::instrument::Instrumented;
use tracing::{debug, trace, Instrument};

pub mod conn;
// use self::connecting::Connecting;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A transport protocol for serving connections.
///
/// This is not meant to be the "accept" part of a server, but instead the connection
/// management and serving part.
pub trait Protocol<S, IO> {
    /// The error when a connection has a problem.
    type Error: Into<Box<dyn std::error::Error + Send + Sync + 'static>>;

    /// The connection future, used to drive a connection IO to completion.
    type Connection: Connection<Self::Error> + Send + 'static;

    /// Serve a connection with upgrades.
    ///
    /// Implementing this method i
    fn serve_connection_with_upgrades(&self, stream: IO, service: S) -> Self::Connection;
}

/// A server that can accept connections, and run each connection
/// using a [tower::Service].
pub struct Server<S, P, A>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>>,
{
    incoming: A,
    make_service: S,
    protocol: P,
}
impl<S, P, A> fmt::Debug for Server<S, P, A>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server").finish()
    }
}
impl<S, P, A> Server<S, P, A>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>>,
{
    /// Set the protocol to use for serving connections.
    pub fn with_protocol<P2>(self, protocol: P2) -> Server<S, P2, A> {
        Server {
            incoming: self.incoming,
            make_service: self.make_service,
            protocol,
        }
    }

    /// Create a new server with the given `MakeService` and `Acceptor`, and a custom [Protocol].
    ///
    /// The default protocol is [AutoBuilder], which can serve both HTTP/1 and HTTP/2 connections,
    /// and will automatically detect the protocol used by the client.
    pub fn new_with_protocol(incoming: A, make_service: S, protocol: P) -> Self {
        Self {
            incoming,
            make_service,
            protocol,
        }
    }

    /// Shutdown the server gracefully when the given future resolves.
    pub fn with_graceful_shutdown<F>(self, signal: F) -> GracefulShutdown<S, P, A, F>
    where
        S: MakeService<(), hyper::Request<hyper::body::Incoming>, Response = crate::body::Response>,
        S::Service: Clone + Send + 'static,
        S::Error: std::error::Error + Send + Sync + 'static,
        S::MakeError: std::error::Error + Send + Sync + 'static,
        <S::Service as tower::Service<hyper::Request<hyper::body::Incoming>>>::Future:
            Send + 'static,
        P: Protocol<S::Service, A::Conn>,
        A: Accept + Unpin,
        A::Conn: HasConnectionInfo + Send + 'static,
        A::Error: std::error::Error + Send + Sync + 'static,
        F: Future<Output = ()> + Send + 'static,
    {
        GracefulShutdown::new(self, signal)
    }
}

impl<S, A> Server<S, AutoBuilder<TokioExecutor>, A>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>>,
{
    /// Create a new server with the given `MakeService` and `Acceptor`.
    pub fn new(incoming: A, make_service: S) -> Self {
        Self {
            incoming,
            make_service,
            protocol: AutoBuilder::new(TokioExecutor::new()),
        }
    }
}

impl<S, P, A> IntoFuture for Server<S, P, A>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>, Response = crate::body::Response>,
    S::Service: Clone + Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    S::MakeError: std::error::Error + Send + Sync + 'static,
    <S::Service as tower::Service<hyper::Request<hyper::body::Incoming>>>::Future: Send + 'static,
    P: Protocol<S::Service, A::Conn>,
    A: Accept + Unpin,
    A::Conn: HasConnectionInfo + Send + 'static,
    A::Error: std::error::Error + Send + Sync + 'static,
{
    type IntoFuture = Serving<S, P, A>;
    type Output = Result<(), ServerError<S::MakeError, A::Error>>;

    fn into_future(self) -> Self::IntoFuture {
        Serving {
            server: self,
            state: State::Preparing,
        }
    }
}

/// A future that drives the server to accept connections.
#[derive(Debug)]
#[pin_project::pin_project]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Serving<S, P, A>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>>,
{
    server: Server<S, P, A>,

    #[pin]
    state: State<S::Service, S::Future>,
}

#[derive(Debug)]
#[pin_project::pin_project(project = StateProj, project_replace = StateProjOwn)]
enum State<S, F> {
    Preparing,
    Accepting {
        service: S,
    },
    Making {
        #[pin]
        future: F,
    },
}

impl<S, P, A> Serving<S, P, A>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>, Response = crate::body::Response>,
    S::Service: Clone + Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    S::MakeError: std::error::Error + Send + Sync + 'static,
    <S::Service as tower::Service<hyper::Request<hyper::body::Incoming>>>::Future: Send + 'static,
    P: Protocol<S::Service, A::Conn>,
    A: Accept + Unpin,
    A::Conn: HasConnectionInfo + Send + 'static,
    A::Error: std::error::Error + Send + Sync + 'static,
{
    /// Polls the server to accept a single new connection.
    ///
    /// The returned connection should be spawned on the runtime.
    #[allow(clippy::type_complexity)]
    fn poll_once(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<Instrumented<P::Connection>>, ServerError<S::MakeError, A::Error>>>
    {
        let mut me = self.as_mut().project();

        match me.state.as_mut().project() {
            StateProj::Preparing => {
                ready!(me.server.make_service.poll_ready(cx)).map_err(ServerError::ready)?;
                let future = me.server.make_service.make_service(());
                me.state.set(State::Making { future });
            }
            StateProj::Making { future, .. } => {
                let service = ready!(future.poll(cx)).map_err(ServerError::make)?;
                trace!("Server is ready to accept");
                me.state.set(State::Accepting { service });
            }
            StateProj::Accepting { .. } => {
                match ready!(Pin::new(&mut me.server.incoming).poll_accept(cx)) {
                    Ok(stream) => {
                        trace!("accepted connection from {}", stream.info().remote_addr());

                        if let StateProjOwn::Accepting { service } =
                            me.state.project_replace(State::Preparing)
                        {
                            let span = tracing::span!(tracing::Level::TRACE, "connection", remote = %stream.info().remote_addr());
                            let conn = me
                                .server
                                .protocol
                                .serve_connection_with_upgrades(stream, service)
                                .instrument(span);

                            return Poll::Ready(Ok(Some(conn)));
                        } else {
                            unreachable!("state must still be accepting");
                        }
                    }
                    Err(e) => {
                        debug!("accept error: {}", e);
                        return Poll::Ready(Err(ServerError::accept(e)));
                    }
                }
            }
        };
        Poll::Ready(Ok(None))
    }
}

impl<S, P, A> Future for Serving<S, P, A>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>, Response = crate::body::Response>,
    S::Service: Clone + Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    S::MakeError: std::error::Error + Send + Sync + 'static,
    <S::Service as tower::Service<hyper::Request<hyper::body::Incoming>>>::Future: Send + 'static,
    P: Protocol<S::Service, A::Conn>,
    A: Accept + Unpin,
    A::Conn: HasConnectionInfo + Send + 'static,
    A::Error: std::error::Error + Send + Sync + 'static,
{
    type Output = Result<(), ServerError<S::MakeError, A::Error>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.as_mut().poll_once(cx) {
                Poll::Ready(Ok(Some(conn))) => {
                    tokio::spawn(async move {
                        if let Err(error) = conn.await {
                            debug!("connection error: {}", error.into());
                        }
                    });
                }
                Poll::Ready(Ok(None)) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[derive(Debug, Clone)]
struct CloseSender(Option<tokio::sync::watch::Receiver<()>>);

impl CloseSender {
    fn send(&mut self) {
        let _ = self.0.take();
        tracing::trace!("sending close signal");
    }
}

#[derive(Debug, Clone)]
struct CloseReciever(Arc<tokio::sync::watch::Sender<()>>);

impl IntoFuture for CloseReciever {
    type IntoFuture = CloseFuture;
    type Output = ();

    fn into_future(self) -> Self::IntoFuture {
        CloseFuture(Box::pin(async move {
            self.0.closed().await;
        }))
    }
}

#[pin_project::pin_project]
struct CloseFuture(#[pin] BoxFuture<'static, ()>);

impl Future for CloseFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().0.poll(cx)
    }
}

fn close() -> (CloseSender, CloseReciever) {
    let (tx, rx) = tokio::sync::watch::channel(());
    (CloseSender(Some(rx)), CloseReciever(Arc::new(tx)))
}

/// A server that can accept connections, and run each connection, and can also process graceful shutdown signals.
#[pin_project::pin_project]
pub struct GracefulShutdown<S, P, A, F>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>>,
{
    #[pin]
    server: Serving<S, P, A>,

    #[pin]
    signal: F,

    channel: CloseReciever,
    shutdown: CloseSender,

    #[pin]
    finished: CloseFuture,
    connection: CloseSender,
}

impl<S, P, A, F> GracefulShutdown<S, P, A, F>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>, Response = crate::body::Response>,
    S::Service: Clone + Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    S::MakeError: std::error::Error + Send + Sync + 'static,
    <S::Service as tower::Service<hyper::Request<hyper::body::Incoming>>>::Future: Send + 'static,
    P: Protocol<S::Service, A::Conn>,
    A: Accept + Unpin,
    A::Conn: HasConnectionInfo + Send + 'static,
    A::Error: std::error::Error + Send + Sync + 'static,
    F: Future<Output = ()>,
{
    fn new(server: Server<S, P, A>, signal: F) -> Self {
        let (tx, rx) = close();
        let (tx2, rx2) = close();
        Self {
            server: server.into_future(),
            signal,
            channel: rx,
            shutdown: tx,
            finished: rx2.into_future(),
            connection: tx2,
        }
    }
}

impl<S, P, A, F> fmt::Debug for GracefulShutdown<S, P, A, F>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GracefulShutdown").finish()
    }
}

impl<S, P, A, F> Future for GracefulShutdown<S, P, A, F>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>, Response = crate::body::Response>,
    S::Service: Clone + Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    S::MakeError: std::error::Error + Send + Sync + 'static,
    <S::Service as tower::Service<hyper::Request<hyper::body::Incoming>>>::Future: Send + 'static,
    P: Protocol<S::Service, A::Conn>,
    A: Accept + Unpin,
    A::Conn: HasConnectionInfo + Send + 'static,
    A::Error: std::error::Error + Send + Sync + 'static,
    F: Future<Output = ()>,
{
    type Output = Result<(), ServerError<S::MakeError, A::Error>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        loop {
            match this.signal.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    debug!("received shutdown signal");
                    this.shutdown.send();
                    return Poll::Ready(Ok(()));
                }
                Poll::Pending => {}
            }

            match this.server.as_mut().poll_once(cx) {
                Poll::Ready(Ok(Some(conn))) => {
                    let shutdown_rx = this.channel.clone();
                    let mut finished_tx = this.connection.clone();

                    tokio::spawn(async move {
                        let shutdown = pin!(shutdown_rx
                            .into_future()
                            .fuse()
                            .instrument(conn.span().clone()));
                        let mut conn = pin!(conn);
                        tokio::select! {
                            rv = &mut conn => {
                                if let Err(error) = rv {
                                    debug!("connection error: {}", error.into());
                                }
                                debug!("connection closed");
                            },
                            _ = shutdown => {
                                debug!("connection received shutdown signal");
                                conn.inner_pin_mut().graceful_shutdown();
                            },
                        }
                        finished_tx.send();
                    });
                }
                Poll::Ready(Ok(None)) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }

            match this.finished.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    debug!("all connections closed");
                    return Poll::Ready(Ok(()));
                }
                Poll::Pending => {}
            }
        }
    }
}

/// An error that can occur when serving connections.
///
/// This error is only returned at the end of the server. Individual connection's
/// errors are discarded and not returned. To handle an individual connection's
/// error, apply a middleware which can process that error in the Service.
#[derive(Debug, thiserror::Error)]
pub enum ServerError<E, A> {
    /// Accept Error
    #[error("accept error: {0}")]
    Accept(#[source] A),

    /// IO Errors
    #[error(transparent)]
    Io(#[from] io::Error),

    /// Errors from the MakeService part of the server.
    #[error("make service: {0}")]
    MakeService(#[source] E),
}

impl<E: std::error::Error, A: std::error::Error> ServerError<E, A> {
    fn accept(error: A) -> Self {
        debug!("accept error: {}", error);
        Self::Accept(error)
    }

    fn make(error: E) -> Self {
        debug!("make service error: {}", error);
        Self::MakeService(error)
    }

    fn ready(error: E) -> Self {
        debug!("ready error: {}", error);
        Self::MakeService(error)
    }
}

#[cfg(test)]
mod tests {}
