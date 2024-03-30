//! Platter
//!
//! A server framework to interoperate with hyper-v1, tokio, braid and arnold
// #![warn(missing_docs)]
#![warn(missing_debug_implementations)]
#![deny(unsafe_code)]

use std::future::Future;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::{fmt, io};

use self::conn::auto::Builder;
use braid::server::{Accept, Stream};
use bridge::rt::TokioExecutor;
use tower::make::MakeService;
use tracing::{debug, trace};

pub mod conn;
mod rewind;
// use self::connecting::Connecting;

/// A transport protocol for serving connections.
///
/// This is not meant to be the "accept" part of a server, but instead the connection
/// management and serving part.
pub trait Protocol<S> {
    /// The error when a connection has a problem.
    type Error: Into<Box<dyn std::error::Error + Send + Sync + 'static>>;

    /// The connection future, used to drive a connection IO to completion.
    type Connection: Future<Output = Result<(), Self::Error>> + Send + 'static;

    /// Serve a connection with upgrades.
    ///
    /// Implementing this method i
    fn serve_connection_with_upgrades(&self, stream: Stream, service: S) -> Self::Connection;
}

/// A server that can accept connections, and run each connection
/// using a [tower::Service].
#[pin_project::pin_project]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Server<S, P>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>>,
{
    incoming: braid::server::Acceptor,
    make_service: S,
    protocol: P,

    #[pin]
    future: State<S::Service, S::Future>,
}

impl<S, P> Server<S, P>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>>,
{
    /// Set the protocol to use for serving connections.
    pub fn with_protocol<P2>(self, protocol: P2) -> Server<S, P2> {
        Server {
            incoming: self.incoming,
            make_service: self.make_service,
            protocol,
            future: State::Preparing,
        }
    }
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

impl<S> Server<S, Builder<TokioExecutor>>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>>,
{
    /// Create a new server with the given `MakeService` and `Acceptor`.
    pub fn new(incoming: braid::server::Acceptor, make_service: S) -> Self {
        Self {
            incoming,
            make_service,
            protocol: Builder::new(TokioExecutor::new()),
            future: State::Preparing,
        }
    }
}

impl<S, P> Server<S, P>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>, Response = arnold::Response>,
    S::Service: Clone + Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    S::MakeError: std::error::Error + Send + Sync + 'static,
    <S::Service as tower::Service<hyper::Request<hyper::body::Incoming>>>::Future: Send + 'static,
    P: Protocol<S::Service>,
{
    /// Polls the server to accept a single new connection.
    ///
    /// The returned connection should be spawned on the runtime.
    #[allow(clippy::type_complexity)]
    fn poll_once(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<P::Connection>, ServerError<S::MakeError>>> {
        let mut me = self.as_mut().project();

        match me.future.as_mut().project() {
            StateProj::Preparing => {
                ready!(me.make_service.poll_ready(cx)).map_err(ServerError::ready)?;
                let future = me.make_service.make_service(());
                me.future.set(State::Making { future });
            }
            StateProj::Making { future, .. } => {
                let service = ready!(future.poll(cx)).map_err(ServerError::make)?;
                trace!("Server is ready to accept");
                me.future.set(State::Accepting { service });
            }
            StateProj::Accepting { .. } => match ready!(Pin::new(me.incoming).poll_accept(cx)) {
                Ok(stream) => {
                    trace!("accepted connection from {}", stream.remote_addr());

                    if let StateProjOwn::Accepting { service } =
                        me.future.project_replace(State::Preparing)
                    {
                        let conn = me.protocol.serve_connection_with_upgrades(stream, service);

                        return Poll::Ready(Ok(Some(conn)));
                    } else {
                        unreachable!("state must still be accepting");
                    }
                }
                Err(e) => {
                    debug!("accept error: {}", e);
                    return Poll::Ready(Err(e.into()));
                }
            },
        };
        Poll::Ready(Ok(None))
    }
}

impl<S, P> fmt::Debug for Server<S, P>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server").finish()
    }
}

impl<S, P> Future for Server<S, P>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>, Response = arnold::Response>,
    S::Service: Clone + Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    S::MakeError: std::error::Error + Send + Sync + 'static,
    <S::Service as tower::Service<hyper::Request<hyper::body::Incoming>>>::Future: Send + 'static,
    P: Protocol<S::Service>,
{
    type Output = Result<(), ServerError<S::MakeError>>;

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

/// An error that can occur when serving connections.
///
/// This error is only returned at the end of the server. Individual connection's
/// errors are discarded and not returned. To handle an individual connection's
/// error, apply a middleware which can process that error in the Service.
#[derive(Debug, thiserror::Error)]
pub enum ServerError<E> {
    /// IO Errors
    #[error(transparent)]
    Io(#[from] io::Error),

    /// Errors from the MakeService part of the server.
    #[error("make service: {0}")]
    MakeService(#[source] E),
}

impl<E: std::error::Error> ServerError<E> {
    fn make(error: E) -> Self {
        debug!("make service error: {}", error);
        Self::MakeService(error)
    }

    fn ready(error: E) -> Self {
        debug!("ready error: {}", error);
        Self::MakeService(error)
    }
}
