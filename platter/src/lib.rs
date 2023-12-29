//! Platter
//!
//! A server framework to interoperate with hyper-v1, tokio, braid and arnold

use std::future::Future;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::{fmt, io};

use braid::server::Accept;
use hyper_util::rt::TokioExecutor;
use hyper_util::server::conn::auto::Builder;
use tower::make::MakeService;
use tracing::debug;

mod connecting;
use self::connecting::Connecting;

#[pin_project::pin_project]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Server<S>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>>,
{
    incoming: braid::server::acceptor::Acceptor,
    make_service: S,
    protocol: Builder<TokioExecutor>,

    #[pin]
    future: State<S::Service, S::Future>,
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

impl<S> Server<S>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>>,
{
    pub fn new(incoming: braid::server::acceptor::Acceptor, make_service: S) -> Self {
        Self {
            incoming,
            make_service,
            protocol: Builder::new(TokioExecutor::new()),
            future: State::Preparing,
        }
    }
}

impl<S> Server<S>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>, Response = arnold::Response>,
    S::Service: Clone + Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    S::MakeError: std::error::Error + Send + Sync + 'static,
    <S::Service as tower::Service<hyper::Request<hyper::body::Incoming>>>::Future: Send + 'static,
{
    /// Polls the server to accept a single new connection.
    ///
    /// The returned connection should be spawned on the runtime.
    #[allow(clippy::type_complexity)]
    fn poll_once(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<Connecting<S::Service>>, ServerError<S::MakeError>>> {
        let mut me = self.as_mut().project();

        match me.future.as_mut().project() {
            StateProj::Preparing => {
                ready!(me.make_service.poll_ready(cx)).map_err(ServerError::ready)?;
                let future = me.make_service.make_service(());
                me.future.set(State::Making { future });
            }
            StateProj::Making { future, .. } => {
                let service = ready!(future.poll(cx)).map_err(ServerError::make)?;
                me.future.set(State::Accepting { service });
            }
            StateProj::Accepting { .. } => match ready!(Pin::new(me.incoming).poll_accept(cx)) {
                Ok(stream) => {
                    if let StateProjOwn::Accepting { service } =
                        me.future.project_replace(State::Preparing)
                    {
                        return Poll::Ready(Ok(Some(Connecting::build(
                            me.protocol.clone(),
                            service,
                            stream,
                        ))));
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

impl<S> fmt::Debug for Server<S>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server").finish()
    }
}

impl<S> Future for Server<S>
where
    S: MakeService<(), hyper::Request<hyper::body::Incoming>, Response = arnold::Response>,
    S::Service: Clone + Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    S::MakeError: std::error::Error + Send + Sync + 'static,
    <S::Service as tower::Service<hyper::Request<hyper::body::Incoming>>>::Future: Send + 'static,
{
    type Output = Result<(), ServerError<S::MakeError>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.as_mut().poll_once(cx) {
                Poll::Ready(Ok(Some(conn))) => {
                    tokio::spawn(conn);
                }
                Poll::Ready(Ok(None)) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError<E> {
    #[error(transparent)]
    Io(#[from] io::Error),

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
