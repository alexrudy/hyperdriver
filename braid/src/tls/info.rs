use core::fmt;
use std::{io, ops::Deref, sync::Arc};
use tokio::sync::RwLock;

use crate::info::{ConnectionInfo, Protocol, SocketAddr};

/// Information about a TLS connection.
#[derive(Debug, Clone)]
pub struct TlsConnectionInfo {
    /// The server name used for this connection, as provided by the SNI
    /// extension.
    pub server_name: Option<String>,

    /// Whether the server name was validated against the certificate.
    pub validated_server_name: bool,

    /// The application layer protocol negotiated for this connection.
    pub alpn: Option<Protocol>,
}

impl TlsConnectionInfo {
    pub(crate) fn channel(
        info: ConnectionInfo,
    ) -> (TlsConnectionInfoSender, TlsConnectionInfoReciever) {
        let (tx, rx) = tokio::sync::oneshot::channel();

        (
            TlsConnectionInfoSender::new(tx, info.clone()),
            TlsConnectionInfoReciever::new(rx, info),
        )
    }

    pub(crate) fn server(server_info: &rustls::ServerConnection) -> Self {
        let server_name = server_info
            .server_name()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        let alpn = server_info
            .alpn_protocol()
            .and_then(|s| std::str::from_utf8(s).ok())
            .and_then(|s| s.parse().ok());

        Self {
            server_name,
            validated_server_name: false,
            alpn,
        }
    }

    pub(crate) fn client(client_info: &rustls::ClientConnection) -> Self {
        let alpn = client_info
            .alpn_protocol()
            .and_then(|s| std::str::from_utf8(s).ok())
            .and_then(|s| s.parse().ok());

        Self {
            server_name: None,
            validated_server_name: false,
            alpn,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn validated(&mut self) {
        self.validated_server_name = true;
    }
}

#[derive(Debug)]
enum RxState {
    Handshake(tokio::sync::oneshot::Receiver<TlsConnectionInfo>),
    Received(TlsConnectionInfo),
}

impl RxState {
    async fn recv(&mut self) -> io::Result<TlsConnectionInfo> {
        let rx = match self {
            RxState::Handshake(rx) => rx,
            RxState::Received(info) => {
                return Ok(info.clone());
            }
        };

        let info = rx
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::NotConnected, "connection was dropped"))?;

        *self = RxState::Received(info.clone());
        Ok(info)
    }
}

/// A receiver for TLS connection info.
#[derive(Debug, Clone)]
pub(crate) struct TlsConnectionInfoReciever {
    state: Arc<RwLock<RxState>>,
    info: ConnectionInfo,
}

impl TlsConnectionInfoReciever {
    /// Create a new TLS connection info receiver.
    pub(crate) fn new(
        inner: tokio::sync::oneshot::Receiver<TlsConnectionInfo>,
        info: ConnectionInfo,
    ) -> Self {
        Self {
            state: Arc::new(RwLock::new(RxState::Handshake(inner))),
            info,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn info(&self) -> ConnectionInfo {
        if let Ok(state) = self.state.try_read() {
            if let RxState::Received(info) = state.deref() {
                return self.info.clone().tls(info.clone());
            }
        }

        self.info.clone()
    }

    /// Get the local address for this connection.
    pub fn local_addr(&self) -> &SocketAddr {
        self.info.local_addr()
    }

    /// Get the remote address for this connection.
    pub fn remote_addr(&self) -> &SocketAddr {
        self.info.remote_addr()
    }

    /// Receive the TLS connection info.
    ///
    /// This will wait until the handshake completes,
    /// and return the underlying connection info.
    pub(crate) async fn recv(&self) -> io::Result<ConnectionInfo> {
        {
            let state = self.state.read().await;

            match state.deref() {
                RxState::Handshake(_) => {}
                RxState::Received(info) => return Ok(self.info.clone().tls(info.clone())),
            };
        }

        let mut state = self.state.write().await;

        state.recv().await.map(|tls| self.info.clone().tls(tls))
    }
}

enum TxState {
    Handshake(tokio::sync::oneshot::Sender<TlsConnectionInfo>),
    Sent(TlsConnectionInfo),
}

/// A sender for TLS connection info.
pub(crate) struct TlsConnectionInfoSender {
    state: TxState,
    info: ConnectionInfo,
}

#[allow(dead_code)]
impl TlsConnectionInfoSender {
    pub(crate) fn new(
        tx: tokio::sync::oneshot::Sender<TlsConnectionInfo>,
        info: ConnectionInfo,
    ) -> Self {
        Self {
            state: TxState::Handshake(tx),
            info,
        }
    }

    pub(crate) fn send(&mut self, info: TlsConnectionInfo) {
        let state = std::mem::replace(&mut self.state, TxState::Sent(info.clone()));
        if let TxState::Handshake(tx) = state {
            let _ = tx.send(info);
        }
    }

    /// Get the local address for this connection.
    pub(crate) fn local_addr(&self) -> &SocketAddr {
        self.info.local_addr()
    }

    /// Get the remote address for this connection.
    pub(crate) fn remote_addr(&self) -> &SocketAddr {
        self.info.remote_addr()
    }

    /// Get the connection info for this connection.
    pub(crate) fn info(&self) -> ConnectionInfo {
        match &self.state {
            TxState::Handshake(_) => self.info.clone(),
            TxState::Sent(tls) => self.info.clone().tls(tls.clone()),
        }
    }
}

impl fmt::Debug for TlsConnectionInfoSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("TlsConnectionInfoSender {{ ...}}")
    }
}
