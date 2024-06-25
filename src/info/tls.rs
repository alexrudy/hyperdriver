//! Information about a TLS connection.
//!
//! TLS information is a bit tricky, because it is realy only available after the handshake is complete.

use super::HasConnectionInfo;
use crate::info::Protocol;

#[cfg(feature = "server")]
pub(crate) use self::channel::{channel, TlsConnectionInfoReciever, TlsConnectionInfoSender};

/// Information about a TLS connection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    #[cfg(feature = "server")]
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

    #[cfg(feature = "client")]
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

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn new_server(
        server_name: Option<String>,
        validated_server_name: bool,
        alpn: Option<Protocol>,
    ) -> Self {
        Self {
            server_name,
            validated_server_name,
            alpn,
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn new_client(alpn: Option<Protocol>) -> Self {
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

/// A trait for types that can provide information about a TLS connection.
pub trait HasTlsConnectionInfo: HasConnectionInfo {
    /// Returns information about the TLS connection, if available.
    ///
    /// This method is async because the information may not be available
    /// and so the handshake may have to complete first.
    fn tls_info(&self) -> Option<&TlsConnectionInfo>;
}

#[cfg(feature = "server")]
mod channel {
    use std::{
        ops::{Deref, DerefMut},
        sync::Arc,
    };
    use tokio::sync::RwLock;

    use crate::info::TlsConnectionInfo;

    pub fn channel() -> (TlsConnectionInfoSender, TlsConnectionInfoReciever) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        (
            TlsConnectionInfoSender { tx: Some(tx) },
            TlsConnectionInfoReciever::new(rx),
        )
    }

    #[derive(Debug)]
    pub struct TlsConnectionInfoSender {
        tx: Option<tokio::sync::oneshot::Sender<TlsConnectionInfo>>,
    }

    impl TlsConnectionInfoSender {
        pub fn send(&mut self, info: TlsConnectionInfo) {
            if let Some(tx) = self.tx.take() {
                let _ = tx.send(info);
            }
        }
    }

    #[derive(Debug)]
    enum State {
        Pending(tokio::sync::oneshot::Receiver<TlsConnectionInfo>),
        Received(TlsConnectionInfo),
        Empty,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct TlsConnectionInfoReciever {
        state: Arc<RwLock<State>>,
    }

    impl TlsConnectionInfoReciever {
        pub(crate) fn empty() -> Self {
            Self {
                state: Arc::new(RwLock::new(State::Empty)),
            }
        }

        pub(crate) fn new(inner: tokio::sync::oneshot::Receiver<TlsConnectionInfo>) -> Self {
            Self {
                state: Arc::new(RwLock::new(State::Pending(inner))),
            }
        }

        pub(crate) async fn recv(&self) -> Option<TlsConnectionInfo> {
            {
                let state = self.state.read().await;

                match state.deref() {
                    State::Pending(_) => {}
                    State::Received(info) => return Some(info.clone()),
                    State::Empty => return None,
                };
            }

            let mut state = self.state.write().await;

            let rx = match state.deref_mut() {
                State::Pending(rx) => rx,
                State::Received(info) => {
                    return Some(info.clone());
                }
                State::Empty => {
                    return None;
                }
            };

            let tls = rx
                .await
                .expect("connection info was never sent and is not available");

            *state = State::Received(tls.clone());
            Some(tls)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    #[test]
    fn tls_server_info() {
        #[derive(Debug, Clone)]
        struct NoCertResolver;

        impl rustls::server::ResolvesServerCert for NoCertResolver {
            fn resolve(
                &self,
                _client_hello: rustls::server::ClientHello,
            ) -> Option<Arc<rustls::sign::CertifiedKey>> {
                None
            }
        }

        let cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(NoCertResolver));

        let server_info = rustls::ServerConnection::new(Arc::new(cfg)).unwrap();
        let info = super::TlsConnectionInfo::server(&server_info);
        assert_eq!(info.server_name, None);
        assert!(!info.validated_server_name);
        assert_eq!(info.alpn, None);
    }

    #[test]
    fn tls_client_info() {
        let root_store = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.into(),
        };

        let cfg = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let client_info =
            rustls::ClientConnection::new(Arc::new(cfg), "example.com".try_into().unwrap())
                .unwrap();
        let info = super::TlsConnectionInfo::client(&client_info);
        assert_eq!(info.server_name, None);
        assert!(!info.validated_server_name);
        assert_eq!(info.alpn, None);
    }
}
