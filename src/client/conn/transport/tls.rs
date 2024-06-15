use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use http::Uri;
use rustls::ClientConfig as TlsClientConfig;

use super::{TlsConnectionError, Transport, TransportStream};
use crate::info::HasConnectionInfo;
use crate::stream::client::Stream as ClientStream;

/// Transport via TLS
#[derive(Debug, Clone)]
pub struct TlsTransportWrapper<T> {
    transport: T,
    config: Arc<TlsClientConfig>,
}

impl<T> TlsTransportWrapper<T> {
    /// Create a new `TlsTransport`
    pub fn new(transport: T, config: Arc<TlsClientConfig>) -> Self {
        Self { transport, config }
    }

    /// Returns the inner transport and the TLS configuration.
    pub fn into_parts(self) -> (T, Arc<TlsClientConfig>) {
        (self.transport, self.config)
    }

    /// Returns a reference to the inner transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Returns a mutable reference to the inner transport.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Returns a reference to the TLS configuration.
    pub fn config(&self) -> &Arc<TlsClientConfig> {
        &self.config
    }
}

impl<T> tower::Service<Uri> for TlsTransportWrapper<T>
where
    T: Transport,
    <T as Transport>::IO: HasConnectionInfo + Unpin,
    <<T as Transport>::IO as HasConnectionInfo>::Addr: Clone + Unpin,
{
    type Response = TransportStream<ClientStream<T::IO>>;
    type Error = TlsConnectionError<T::Error>;
    type Future = future::TlsConnectionFuture<T>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.transport
            .poll_ready(cx)
            .map_err(TlsConnectionError::Connection)
    }

    fn call(&mut self, req: Uri) -> Self::Future {
        let config = self.config.clone();
        let Some(host) = req.host().map(String::from) else {
            return future::TlsConnectionFuture::error(TlsConnectionError::NoDomain);
        };

        let future = self.transport.connect(req);

        future::TlsConnectionFuture::new(future, config, host)
    }
}

pub(in crate::client::conn::transport) mod future {
    use std::fmt;
    use std::future::Future;
    use std::sync::Arc;
    use std::task::{Context, Poll};

    use pin_project::pin_project;

    use crate::info::tls::HasTlsConnectionInfo;

    use super::super::Transport;
    use super::*;

    #[pin_project(project = StateProject, project_replace = StateProjectOwned)]
    enum State<T>
    where
        T: Transport,
    {
        Connecting {
            #[pin]
            future: T::Future,
            config: Arc<TlsClientConfig>,
            domain: String,
        },

        Handshake {
            stream: ClientStream<T::IO>,
        },

        Error {
            error: TlsConnectionError<T::Error>,
        },

        Invalid,
    }

    impl<T: Transport> fmt::Debug for State<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                State::Connecting { .. } => f.debug_struct("Connecting").finish(),
                State::Handshake { .. } => f.debug_struct("Handshake").finish(),
                State::Error { .. } => f.debug_struct("Error").finish(),
                State::Invalid => f.debug_struct("Invalid").finish(),
            }
        }
    }

    #[pin_project]
    #[derive(Debug)]
    pub struct TlsConnectionFuture<T: Transport> {
        #[pin]
        state: State<T>,
    }

    impl<T: Transport> TlsConnectionFuture<T> {
        pub(super) fn new(future: T::Future, config: Arc<TlsClientConfig>, domain: String) -> Self {
            Self {
                state: State::Connecting {
                    future,
                    config,
                    domain,
                },
            }
        }

        pub(super) fn error(error: TlsConnectionError<T::Error>) -> Self {
            Self {
                state: State::Error { error },
            }
        }
    }

    impl<T> Future for TlsConnectionFuture<T>
    where
        T: Transport,
        <T as Transport>::IO: HasConnectionInfo + Unpin,
        <<T as Transport>::IO as HasConnectionInfo>::Addr: Clone + Unpin,
    {
        type Output = Result<TransportStream<ClientStream<T::IO>>, TlsConnectionError<T::Error>>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let mut this = self.project();
            loop {
                match this.state.as_mut().project() {
                    StateProject::Connecting {
                        future,
                        config,
                        domain,
                    } => match future.poll(cx) {
                        Poll::Ready(Ok(stream)) => {
                            let stream = stream.into_inner();
                            let stream = ClientStream::new(stream).tls(domain, config.clone());
                            this.state.set(State::Handshake { stream });
                        }
                        Poll::Ready(Err(e)) => {
                            return Poll::Ready(Err(TlsConnectionError::Connection(e)))
                        }
                        Poll::Pending => return Poll::Pending,
                    },
                    StateProject::Handshake { stream } => match stream.poll_handshake(cx) {
                        Poll::Ready(Ok(())) => {
                            let StateProjectOwned::Handshake { stream } =
                                this.state.project_replace(State::Invalid)
                            else {
                                unreachable!();
                            };

                            let info = stream.info();
                            let tls = stream.tls_info().cloned();

                            return Poll::Ready(Ok(TransportStream { stream, info, tls }));
                        }
                        Poll::Ready(Err(e)) => {
                            return Poll::Ready(Err(TlsConnectionError::Handshake(e)))
                        }
                        Poll::Pending => return Poll::Pending,
                    },
                    StateProject::Error { .. } => {
                        let StateProjectOwned::Error { error } =
                            this.state.project_replace(State::Invalid)
                        else {
                            unreachable!();
                        };

                        return Poll::Ready(Err(error));
                    }
                    StateProject::Invalid => panic!("polled after ready"),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {}
