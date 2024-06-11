//! A server framework to interoperate with hyper-v1, tokio, braid and arnold

#![warn(missing_docs)]
#![warn(missing_debug_implementations)]
#![deny(unsafe_code)]

use std::future::{Future, IntoFuture};
use std::marker::PhantomData;
use std::pin::{pin, Pin};
use std::sync::Arc;
use std::task::{ready, Context, Poll};
use std::{fmt, io};

pub use self::conn::auto::Builder as AutoBuilder;
use self::conn::Connection;
use crate::bridge::rt::TokioExecutor;
use crate::stream::info::HasConnectionInfo;
pub use crate::stream::server::Accept;
use crate::stream::server::Acceptor;
use futures_util::future::FutureExt as _;
use http_body::Body;
use service::MakeServiceRef;
use tower::make::Shared;
use tracing::instrument::Instrumented;
use tracing::{debug, trace, Instrument};

pub mod conn;
pub mod service;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// A transport protocol for serving connections.
///
/// This is not meant to be the "accept" part of a server, but instead the connection
/// management and serving part.
pub trait Protocol<S, IO> {
    /// The error when a connection has a problem.
    type Error: Into<Box<dyn std::error::Error + Send + Sync>>;

    /// The connection future, used to drive a connection IO to completion.
    type Connection: Connection + Future<Output = Result<(), Self::Error>> + Send + 'static;

    /// Serve a connection with possible upgrades.
    ///
    /// Implementing this method does not guarantee that a protocol can be upgraded,
    /// just that we can serve the connection.
    fn serve_connection_with_upgrades(&self, stream: IO, service: S) -> Self::Connection;
}

/// A server that can accept connections, and run each connection
/// using a [tower::Service].
pub struct Server<S, P, A, B> {
    incoming: A,
    make_service: S,
    protocol: P,
    body: PhantomData<fn(B) -> ()>,
}

impl<S, P, A, B> fmt::Debug for Server<S, P, A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server").finish()
    }
}
impl<S, P, A, B> Server<S, P, A, B> {
    /// Set the protocol to use for serving connections.
    pub fn with_protocol<P2>(self, protocol: P2) -> Server<S, P2, A, B> {
        Server {
            incoming: self.incoming,
            make_service: self.make_service,
            protocol,
            body: PhantomData,
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
            body: PhantomData,
        }
    }

    /// Shutdown the server gracefully when the given future resolves.
    pub fn with_graceful_shutdown<F>(self, signal: F) -> GracefulShutdown<S, P, A, B, F>
    where
        S: MakeServiceRef<A::Conn, B>,
        P: Protocol<S::Service, A::Conn>,
        A: Accept + Unpin,
        B: Body,
        F: Future<Output = ()> + Send + 'static,
    {
        GracefulShutdown::new(self, signal)
    }
}

impl<S, A, B> Server<S, AutoBuilder<TokioExecutor>, A, B> {
    /// Create a new server with the given `MakeService` and `Acceptor`.
    pub fn new(incoming: A, make_service: S) -> Self {
        Self {
            incoming,
            make_service,
            protocol: AutoBuilder::new(TokioExecutor::new()),
            body: PhantomData,
        }
    }
}

impl<S, B> Server<S, AutoBuilder<TokioExecutor>, Acceptor<tokio::net::TcpListener>, B> {
    /// Bind a new server to the given address.
    pub async fn bind(addr: std::net::SocketAddr, make_service: S) -> io::Result<Self> {
        let incoming = tokio::net::TcpListener::bind(addr).await?;
        let accept = Acceptor::new(incoming);
        Ok(Server::new(accept, make_service))
    }
}

impl<S, A, B> Server<Shared<S>, AutoBuilder<TokioExecutor>, A, B> {
    /// Create a new server with the given `Service` and `Acceptor`. The service will be cloned for each connection.
    pub fn new_shared(incoming: A, service: S) -> Self {
        Self {
            incoming,
            make_service: Shared::new(service),
            protocol: AutoBuilder::new(TokioExecutor::new()),
            body: PhantomData,
        }
    }
}

impl<S, P, A, B> IntoFuture for Server<S, P, A, B>
where
    S: MakeServiceRef<A::Conn, B>,
    P: Protocol<S::Service, A::Conn>,
    A: Accept + Unpin,
    B: Body,
{
    type IntoFuture = Serving<S, P, A, B>;
    type Output = Result<(), ServerError>;

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
pub struct Serving<S, P, A, B>
where
    S: MakeServiceRef<A::Conn, B>,
    A: Accept,
{
    server: Server<S, P, A, B>,

    #[pin]
    state: State<A::Conn, S::Future>,
}

#[derive(Debug)]
#[pin_project::pin_project(project = StateProj, project_replace = StateProjOwn)]
enum State<S, F> {
    Preparing,
    Accepting,
    Making {
        #[pin]
        future: F,
        stream: S,
    },
}

impl<S, P, A, B> Serving<S, P, A, B>
where
    S: MakeServiceRef<A::Conn, B>,
    P: Protocol<S::Service, A::Conn>,
    A: Accept + Unpin,
{
    /// Polls the server to accept a single new connection.
    ///
    /// The returned connection should be spawned on the runtime.
    #[allow(clippy::type_complexity)]
    fn poll_once(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<Instrumented<P::Connection>>, ServerError>> {
        let mut me = self.as_mut().project();

        match me.state.as_mut().project() {
            StateProj::Preparing => {
                ready!(me.server.make_service.poll_ready_ref(cx)).map_err(ServerError::ready)?;
                me.state.set(State::Accepting);
            }
            StateProj::Accepting => match ready!(Pin::new(&mut me.server.incoming).poll_accept(cx))
            {
                Ok(stream) => {
                    trace!("accepted connection from {}", stream.info().remote_addr());
                    let future = me.server.make_service.make_service_ref(&stream);
                    me.state.set(State::Making { future, stream });
                }
                Err(e) => {
                    return Poll::Ready(Err(ServerError::accept(e)));
                }
            },
            StateProj::Making { future, .. } => {
                let service = ready!(future.poll(cx)).map_err(ServerError::make)?;
                if let StateProjOwn::Making { stream, .. } =
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
        };
        Poll::Ready(Ok(None))
    }
}

impl<S, P, A, B> Future for Serving<S, P, A, B>
where
    S: MakeServiceRef<A::Conn, B>,
    P: Protocol<S::Service, A::Conn>,
    A: Accept + Unpin,
    B: Body,
{
    type Output = Result<(), ServerError>;

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
pub struct GracefulShutdown<S, P, A, B, F>
where
    S: MakeServiceRef<A::Conn, B>,
    A: Accept,
{
    #[pin]
    server: Serving<S, P, A, B>,

    #[pin]
    signal: F,

    channel: CloseReciever,
    shutdown: CloseSender,

    #[pin]
    finished: CloseFuture,
    connection: CloseSender,
}

impl<S, P, A, B, F> GracefulShutdown<S, P, A, B, F>
where
    S: MakeServiceRef<A::Conn, B>,
    P: Protocol<S::Service, A::Conn>,
    A: Accept + Unpin,
    B: Body,
    F: Future<Output = ()>,
{
    fn new(server: Server<S, P, A, B>, signal: F) -> Self {
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

impl<S, P, A, B, F> fmt::Debug for GracefulShutdown<S, P, A, B, F>
where
    S: MakeServiceRef<A::Conn, B>,
    A: Accept,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GracefulShutdown").finish()
    }
}

impl<S, P, A, Body, F> Future for GracefulShutdown<S, P, A, Body, F>
where
    S: MakeServiceRef<A::Conn, Body>,
    P: Protocol<S::Service, A::Conn>,
    A: Accept + Unpin,
    F: Future<Output = ()>,
{
    type Output = Result<(), ServerError>;

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
                                //TODO: "connection should continued to be polled until it is finished"
                                // conn.await;
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
pub enum ServerError {
    /// Accept Error
    #[error("accept error: {0}")]
    Accept(#[source] BoxError),

    /// IO Errors
    #[error(transparent)]
    Io(#[from] io::Error),

    /// Errors from the MakeService part of the server.
    #[error("make service: {0}")]
    MakeService(#[source] BoxError),
}

impl ServerError {
    fn accept<A>(error: A) -> Self
    where
        A: Into<BoxError>,
    {
        let boxed = error.into();
        debug!("accept error: {}", boxed);
        Self::Accept(boxed)
    }
}

impl ServerError {
    fn make<E>(error: E) -> Self
    where
        E: Into<BoxError>,
    {
        let boxed = error.into();
        debug!("make service error: {}", boxed);
        Self::MakeService(boxed)
    }

    fn ready<E>(error: E) -> Self
    where
        E: Into<BoxError>,
    {
        let boxed = error.into();
        debug!("ready error: {}", boxed);
        Self::MakeService(boxed)
    }
}

#[cfg(test)]
mod tests {}
