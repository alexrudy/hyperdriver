use std::fmt;

use ::http::{Response, Version};
use braid::client::Stream;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use thiserror::Error;
use tracing::trace;

use crate::{conn::tcp::TcpConnectionError, pool::Poolable};

use super::{tcp, ConnectionProtocol};

pub struct Connection {
    inner: InnerConnection,
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConnection")
            .field("version", &self.version())
            .finish()
    }
}

impl Connection {
    pub(crate) async fn send_request(
        &mut self,
        request: arnold::Request,
    ) -> Result<Response<Incoming>, hyper::Error> {
        match &mut self.inner {
            InnerConnection::H2(conn) => conn.send_request(request).await,
            InnerConnection::H1(conn) => conn.send_request(request).await,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn version(&self) -> Version {
        match &self.inner {
            InnerConnection::H2(_) => Version::HTTP_2,
            InnerConnection::H1(_) => Version::HTTP_11,
        }
    }

    pub(crate) async fn when_ready(&mut self) -> Result<(), hyper::Error> {
        match &mut self.inner {
            InnerConnection::H2(conn) => conn.ready().await,
            InnerConnection::H1(conn) => conn.ready().await,
        }
    }
}

enum InnerConnection {
    H2(hyper::client::conn::http2::SendRequest<arnold::Body>),
    H1(hyper::client::conn::http1::SendRequest<arnold::Body>),
}

impl Poolable for Connection {
    fn is_open(&self) -> bool {
        match &self.inner {
            InnerConnection::H2(ref conn) => conn.is_ready(),
            InnerConnection::H1(ref conn) => conn.is_ready(),
        }
    }

    fn can_share(&self) -> bool {
        match &self.inner {
            InnerConnection::H2(_) => true,
            InnerConnection::H1(_) => false,
        }
    }

    fn reuse(&mut self) -> Option<Self> {
        match &self.inner {
            InnerConnection::H2(conn) => Some(Self {
                inner: InnerConnection::H2(conn.clone()),
            }),
            InnerConnection::H1(_) => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error(transparent)]
    Connecting(#[from] tcp::TcpConnectionError),

    #[error("handshake: {0}")]
    Handshake(#[source] hyper::Error),

    #[error("connection cancelled")]
    Canceled(#[source] hyper::Error),

    #[error("connection closed")]
    Closed(#[source] hyper::Error),

    #[error("connection timeout")]
    Timeout,
}

#[derive(Debug, Clone)]
pub struct Builder {
    http1: hyper::client::conn::http1::Builder,
    http2: hyper::client::conn::http2::Builder<TokioExecutor>,
    pub(crate) protocol: ConnectionProtocol,
}

impl Builder {
    pub fn set_protocol(&mut self, protocol: ConnectionProtocol) -> &mut Self {
        self.protocol = protocol;
        self
    }

    pub fn http1(&mut self) -> &mut hyper::client::conn::http1::Builder {
        &mut self.http1
    }

    pub fn http2(&mut self) -> &mut hyper::client::conn::http2::Builder<TokioExecutor> {
        &mut self.http2
    }
}

impl Builder {
    async fn handshake_h2(&self, stream: Stream) -> Result<Connection, ConnectionError> {
        trace!("handshake http2");
        let (sender, conn) = self
            .http2
            .handshake(TokioIo::new(stream))
            .await
            .map_err(ConnectionError::Handshake)?;
        tokio::spawn(async {
            if let Err(err) = conn.await {
                tracing::error!(%err, "h2 connection error");
            }
        });
        Ok(Connection {
            inner: InnerConnection::H2(sender),
        })
    }

    async fn handshake_h1(&self, stream: Stream) -> Result<Connection, ConnectionError> {
        trace!("handshake http1");
        let (sender, conn) = self
            .http1
            .handshake(TokioIo::new(stream))
            .await
            .map_err(ConnectionError::Handshake)?;
        tokio::spawn(async {
            if let Err(err) = conn.await {
                tracing::error!(%err, "h1 connection error");
            }
        });
        Ok(Connection {
            inner: InnerConnection::H1(sender),
        })
    }

    pub(crate) async fn handshake(
        &self,
        mut stream: Stream,
    ) -> Result<Connection, ConnectionError> {
        match self.protocol {
            ConnectionProtocol::Http2 => self.handshake_h2(stream).await,
            ConnectionProtocol::Http1 => {
                stream.finish_handshake().await.map_err(|error| {
                    ConnectionError::Connecting(TcpConnectionError::msg("tls handshake error")(
                        error,
                    ))
                })?;

                let info = stream.info().await.map_err(|error| {
                    ConnectionError::Connecting(TcpConnectionError::msg("tls info error")(error))
                })?;

                if info.tls.as_ref().and_then(|tls| tls.alpn.as_ref())
                    == Some(&braid::info::Protocol::Http(http::Version::HTTP_2))
                {
                    trace!("alpn h2 switching");
                    //TODO: There should be some way to multiplex at this point.
                    // One strategy: separate the transport and protocol pieces,
                    // and allow the pool to introspect the transport to see if we
                    // are doing ALPN.
                    return self.handshake_h2(stream).await;
                }

                self.handshake_h1(stream).await
            }
        }
    }
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            http1: hyper::client::conn::http1::Builder::new(),
            http2: hyper::client::conn::http2::Builder::new(TokioExecutor::new()),
            protocol: ConnectionProtocol::Http1,
        }
    }
}
