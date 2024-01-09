use http::Uri;
use tracing::{instrument::Instrumented, Instrument};

use super::{Builder, Connection, ConnectionError, TcpConnector, Transport};

#[derive(Debug, Clone)]
pub struct HttpConnector<T = TcpConnector> {
    transport: T,
    builder: Builder,
}

impl<T> HttpConnector<T> {
    pub(crate) fn new(transport: T, builder: Builder) -> Self {
        Self { transport, builder }
    }
}

impl<T> tower::Service<Uri> for HttpConnector<T>
where
    T: Transport + Clone,
{
    type Response = Connection;

    type Error = ConnectionError;

    type Future = Instrumented<future::HttpConnectFuture<T>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.transport.poll_ready(cx).map_err(Into::into)
    }

    #[tracing::instrument("http-connect", skip(self, req), fields(host = %req.host().unwrap_or("-")))]
    fn call(&mut self, req: Uri) -> Self::Future {
        let next = self.transport.clone();
        let transport = std::mem::replace(&mut self.transport, next);

        future::HttpConnectFuture::new(transport, self.builder.clone(), req).in_current_span()
    }
}

mod future {
    use std::fmt;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{ready, Context, Poll};

    use braid::client::Stream;
    use http::Uri;
    use pin_project::pin_project;

    use crate::conn::tcp::TcpConnectionError;
    use crate::conn::{Builder, Connection, ConnectionError};

    struct DebugLiteral<T: fmt::Display>(T);

    impl<T: fmt::Display> fmt::Debug for DebugLiteral<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    type BoxFuture<'a, T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

    #[pin_project(project = StateProj, project_replace = StateProjReplace)]
    enum State<T>
    where
        T: tower::Service<Uri>,
    {
        Error(Option<ConnectionError>),
        Connecting {
            #[pin]
            oneshot: T::Future,
            builder: Builder,
        },
        Handshaking {
            future: BoxFuture<'static, Connection, ConnectionError>,
        },
    }

    #[pin_project]
    pub struct HttpConnectFuture<T>
    where
        T: tower::Service<Uri>,
    {
        #[pin]
        state: State<T>,
    }

    impl<T> fmt::Debug for HttpConnectFuture<T>
    where
        T: tower::Service<Uri>,
    {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let field = match &self.state {
                State::Error(_) => "Error",
                State::Connecting { .. } => "Connecting",
                State::Handshaking { .. } => "Handshaking",
            };

            f.debug_struct("HttpConnectFuture")
                .field("state", &DebugLiteral(field))
                .finish()
        }
    }

    impl<T> HttpConnectFuture<T>
    where
        T: tower::Service<Uri>,
    {
        pub(super) fn new(mut transport: T, builder: Builder, uri: Uri) -> Self {
            Self {
                state: State::Connecting {
                    oneshot: transport.call(uri),
                    builder,
                },
            }
        }

        #[allow(dead_code)]
        pub(super) fn error(err: ConnectionError) -> Self {
            Self {
                state: State::Error(Some(err)),
            }
        }
    }

    impl<T> Future for HttpConnectFuture<T>
    where
        T: tower::Service<Uri, Response = Stream, Error = TcpConnectionError>,
    {
        type Output = Result<Connection, ConnectionError>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let mut this = self.project();
            loop {
                match this.state.as_mut().project() {
                    StateProj::Connecting { oneshot, builder } => {
                        let stream = ready!(oneshot.poll(cx))?;
                        let builder = builder.clone();
                        let future = Box::pin(async move { builder.handshake(stream).await });

                        this.state.set(State::Handshaking { future });
                    }
                    StateProj::Handshaking { future } => {
                        return future.as_mut().poll(cx);
                    }
                    StateProj::Error(error) => {
                        if let Some(error) = error.take() {
                            return Poll::Ready(Err(error));
                        } else {
                            panic!("invalid future state");
                        }
                    }
                }
            }
        }
    }
}
