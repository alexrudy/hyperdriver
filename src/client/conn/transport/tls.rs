use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use http::Uri;
use rustls::ClientConfig as TlsClientConfig;

use super::{TlsConnectionError, Transport, TransportStream};
use crate::client::stream::Stream as ClientStream;
use crate::info::HasConnectionInfo;

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
    <<T as Transport>::IO as HasConnectionInfo>::Addr: Clone + Send + Unpin,
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
    use crate::stream::tls::TlsHandshakeStream as _;

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
        <<T as Transport>::IO as HasConnectionInfo>::Addr: Clone + Send + Unpin,
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
                            tracing::trace!("Transport connected. TLS handshake starting");
                            this.state.set(State::Handshake { stream });
                        }
                        Poll::Ready(Err(e)) => {
                            tracing::trace!(?e, "Transport connection error");
                            return Poll::Ready(Err(TlsConnectionError::Connection(e)));
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

                            tracing::trace!(?info, "TLS handshake complete");
                            return Poll::Ready(Ok(TransportStream { stream, info, tls }));
                        }
                        Poll::Ready(Err(e)) => {
                            tracing::trace!(?e, "Transport handshake error");
                            return Poll::Ready(Err(TlsConnectionError::Handshake(e)));
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
mod tests {

    use tower::ServiceExt;

    use crate::{
        fixtures,
        stream::{
            server::AcceptExt,
            tls::{TlsHandshakeExt, TlsHandshakeStream as _},
        },
    };

    #[tokio::test]
    async fn test_tls_transport_wrapper() {
        let (client, server) = crate::stream::duplex::pair("example.com".parse().unwrap());

        let mut config = fixtures::tls_client_config();
        config.alpn_protocols.push(b"h2".to_vec());
        let transport = crate::client::conn::transport::TlsTransportWrapper::new(
            crate::client::conn::transport::duplex::DuplexTransport::new(1024, None, client),
            config.into(),
        );

        let mut config = fixtures::tls_server_config();
        config.alpn_protocols.push(b"h2".to_vec());
        let accept = crate::stream::server::Acceptor::new(server).with_tls(config.into());

        let uri = "https://example.com/".parse().unwrap();

        let (stream, _) = tokio::join!(
            async {
                let mut stream = transport.oneshot(uri).await.unwrap();
                stream.get_io_mut().finish_handshake().await.unwrap();
                stream
            },
            async move {
                let mut conn = accept.accept().await.unwrap();
                conn.handshake().await.unwrap();
                conn
            }
        );

        assert_eq!(
            stream.tls_info().unwrap().alpn,
            Some(crate::info::Protocol::http(http::Version::HTTP_2))
        );
    }
}
