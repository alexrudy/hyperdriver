//! A server framework to interoperate with hyper-v1, tokio, braid and arnold

use std::future::{Future, IntoFuture};
use std::marker::PhantomData;
use std::pin::{pin, Pin};
use std::sync::Arc;
use std::task::{ready, Context, Poll};
use std::{fmt, io};

use futures_util::future::FutureExt as _;
use http_body::Body;
#[cfg(feature = "stream")]
use tokio::net::ToSocketAddrs;
use tracing::instrument::Instrumented;
use tracing::{debug, Instrument};

pub use self::builder::Builder;
pub use self::conn::auto::Builder as AutoBuilder;
pub use self::conn::Accept;
#[cfg(feature = "stream")]
use self::conn::Acceptor;
use self::conn::Connection;
#[cfg(feature = "stream")]
use crate::bridge::rt::TokioExecutor;
use crate::service::MakeServiceRef;

mod builder;
pub mod conn;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type BoxError = Box<dyn std::error::Error + Send + Sync>;

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
pub struct Server<A, P, S, B> {
    incoming: A,
    protocol: P,
    make_service: S,
    body: PhantomData<fn(B) -> ()>,
}

impl<A, P, S, B> fmt::Debug for Server<A, P, S, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server").finish()
    }
}

impl Server<(), (), (), ()> {
    /// Create a new server builder.
    ///
    ///
    /// To build a simple server, you can use the `with_shared_service` method:
    /// ```rust
    /// use hyperdriver::stream::duplex;
    /// use hyperdriver::Server;
    /// use hyperdriver::Body;
    /// use tower::service_fn;
    ///
    /// #[derive(Debug)]
    /// struct MyError;
    ///
    /// impl std::fmt::Display for MyError {
    ///    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    ///         f.write_str("MyError")
    ///   }
    /// }
    ///
    /// impl std::error::Error for MyError {}
    ///
    /// # async fn example() {
    /// let (_, incoming) = duplex::pair("server.test".parse().unwrap());
    /// let server = Server::builder()
    ///     .with_acceptor(incoming)
    ///     .with_shared_service(service_fn(|req| async move {
    ///        Ok::<_, MyError>(http::Response::new(Body::empty()))
    ///    }))
    ///    .with_auto_http()
    ///    .build();
    ///
    /// server.await.unwrap();
    /// # }

    pub fn builder() -> Builder {
        Builder::new()
    }
}

impl<A, P, S, B> Server<A, P, S, B> {
    /// Create a new server with the given `MakeService` and `Acceptor`, and a custom [Protocol].
    ///
    /// The default protocol is [AutoBuilder], which can serve both HTTP/1 and HTTP/2 connections,
    /// and will automatically detect the protocol used by the client.
    pub fn new(incoming: A, protocol: P, make_service: S) -> Self {
        Self {
            incoming,
            protocol,
            make_service,
            body: PhantomData,
        }
    }

    /// Shutdown the server gracefully when the given future resolves.
    pub fn with_graceful_shutdown<F>(self, signal: F) -> GracefulShutdown<A, P, S, B, F>
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

#[cfg(feature = "stream")]
impl<S, B> Server<Acceptor, AutoBuilder<TokioExecutor>, S, B> {
    /// Bind a new server to the given address.
    pub async fn bind<A: ToSocketAddrs>(addr: A, make_service: S) -> io::Result<Self> {
        let incoming = tokio::net::TcpListener::bind(addr).await?;
        let accept = Acceptor::from(incoming);

        Ok(Server::builder()
            .with_acceptor(accept)
            .with_auto_http()
            .with_make_service(make_service)
            .with_body()
            .build())
    }
}

impl<A, P, S, B> IntoFuture for Server<A, P, S, B>
where
    S: MakeServiceRef<A::Conn, B>,
    P: Protocol<S::Service, A::Conn>,
    A: Accept + Unpin,
    B: Body,
{
    type IntoFuture = Serving<A, P, S, B>;
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
pub struct Serving<A, P, S, B>
where
    S: MakeServiceRef<A::Conn, B>,
    A: Accept,
{
    server: Server<A, P, S, B>,

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

impl<A, P, S, B> Serving<A, P, S, B>
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
                    let span = tracing::span!(tracing::Level::TRACE, "connection");
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

impl<A, P, S, B> Future for Serving<A, P, S, B>
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
                            debug!("connection error: {:?}", error.into());
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
pub struct GracefulShutdown<A, P, S, B, F>
where
    S: MakeServiceRef<A::Conn, B>,
    A: Accept,
{
    #[pin]
    server: Serving<A, P, S, B>,

    #[pin]
    signal: F,

    channel: CloseReciever,
    shutdown: CloseSender,

    #[pin]
    finished: CloseFuture,
    connection: CloseSender,
}

impl<A, P, S, B, F> GracefulShutdown<A, P, S, B, F>
where
    S: MakeServiceRef<A::Conn, B>,
    P: Protocol<S::Service, A::Conn>,
    A: Accept + Unpin,
    B: Body,
    F: Future<Output = ()>,
{
    fn new(server: Server<A, P, S, B>, signal: F) -> Self {
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

impl<A, P, S, B, F> fmt::Debug for GracefulShutdown<A, P, S, B, F>
where
    S: MakeServiceRef<A::Conn, B>,
    A: Accept,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GracefulShutdown").finish()
    }
}

impl<A, P, S, Body, F> Future for GracefulShutdown<A, P, S, Body, F>
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
                        let mut shutdown = pin!(shutdown_rx
                            .into_future()
                            .fuse()
                            .instrument(conn.span().clone()));
                        let mut conn = pin!(conn);
                        loop {
                            tokio::select! {
                                rv = &mut conn.as_mut() => {
                                    if let Err(error) = rv {
                                        debug!("connection error: {}", error.into());
                                    }
                                    debug!("connection closed");
                                    break;
                                },
                                _ = &mut shutdown => {
                                    debug!("connection received shutdown signal");
                                    conn.as_mut().inner_pin_mut().graceful_shutdown();
                                },
                            }
                        }
                        finished_tx.send();
                        tracing::trace!("finished serving connection");
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
mod tests {

    use std::{
        convert::Infallible,
        future::{ready, IntoFuture},
    };

    use super::*;

    use crate::{stream::duplex, Body};

    use crate::service::make_service_fn;
    use tower::service_fn;

    #[allow(dead_code, unused_must_use)]
    fn compile() {
        let svc = make_service_fn(|_| {
            ready(Ok::<_, Infallible>(service_fn(|_: http::Request<Body>| {
                ready(Ok::<_, Infallible>(http::Response::new(crate::Body::from(
                    "hello",
                ))))
            })))
        });

        let (_, incoming) = duplex::pair("server.test".parse().unwrap());

        let server = Server::builder()
            .with_acceptor(incoming)
            .with_make_service(svc)
            .with_auto_http()
            .build();

        server.into_future();
    }
}
