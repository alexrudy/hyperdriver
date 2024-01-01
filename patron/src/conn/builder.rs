use std::fmt;

use ::http::{Response, Version};
use braid::client::Stream;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use thiserror::Error;
use tracing::trace;

use crate::pool::Poolable;

use super::{tcp, ConnectionProtocol};

pub struct ClientConnection {
    inner: InnerClientConnection,
}

impl fmt::Debug for ClientConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConnection")
            .field("version", &self.version())
            .finish()
    }
}

impl ClientConnection {
    pub(crate) async fn send_request(
        &mut self,
        request: arnold::Request,
    ) -> Result<Response<Incoming>, hyper::Error> {
        match &mut self.inner {
            InnerClientConnection::H2(conn) => conn.send_request(request).await,
            InnerClientConnection::H1(conn) => conn.send_request(request).await,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn version(&self) -> Version {
        match &self.inner {
            InnerClientConnection::H2(_) => Version::HTTP_2,
            InnerClientConnection::H1(_) => Version::HTTP_11,
        }
    }

    pub(crate) async fn when_ready(&mut self) -> Result<(), hyper::Error> {
        match &mut self.inner {
            InnerClientConnection::H2(conn) => conn.ready().await,
            InnerClientConnection::H1(conn) => conn.ready().await,
        }
    }
}

enum InnerClientConnection {
    H2(hyper::client::conn::http2::SendRequest<arnold::Body>),
    H1(hyper::client::conn::http1::SendRequest<arnold::Body>),
}

impl Poolable for ClientConnection {
    fn is_open(&self) -> bool {
        match &self.inner {
            InnerClientConnection::H2(ref conn) => conn.is_ready(),
            InnerClientConnection::H1(ref conn) => conn.is_ready(),
        }
    }

    fn can_share(&self) -> bool {
        match &self.inner {
            InnerClientConnection::H2(_) => true,
            InnerClientConnection::H1(_) => false,
        }
    }

    fn reuse(&mut self) -> Option<Self> {
        match &self.inner {
            InnerClientConnection::H2(conn) => Some(Self {
                inner: InnerClientConnection::H2(conn.clone()),
            }),
            InnerClientConnection::H1(_) => None,
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
    pub(crate) async fn handshake(
        &self,
        stream: Stream,
    ) -> Result<ClientConnection, ConnectionError> {
        match self.protocol {
            ConnectionProtocol::Http2 => {
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
                Ok(ClientConnection {
                    inner: InnerClientConnection::H2(sender),
                })
            }
            ConnectionProtocol::Http1 => {
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
                Ok(ClientConnection {
                    inner: InnerClientConnection::H1(sender),
                })
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
