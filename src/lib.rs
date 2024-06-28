//! Hyperdriver
//!
//! Building the missing middle for network services in Rust.

#![cfg_attr(docsrs, feature(doc_auto_cfg))]

use std::fmt;

use tracing::dispatcher;

pub mod body;
pub use body::Body;
pub mod bridge;
#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "client")]
pub use client::Client;
#[cfg(feature = "discovery")]
pub mod discovery;
#[cfg(feature = "client")]
pub(crate) mod happy_eyeballs;
#[cfg(feature = "client")]
mod lazy;
#[cfg(feature = "pidfile")]
pub mod pidfile;
#[cfg(feature = "server")]
mod rewind;
#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "server")]
pub use server::Server;
pub mod info;
pub mod service;
pub mod stream;

#[allow(unused)]
pub(crate) struct DebugLiteral<T: fmt::Display>(T);

impl<T: fmt::Display> fmt::Debug for DebugLiteral<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[allow(unused)]
#[track_caller]
pub(crate) fn polled_span(span: &tracing::Span) {
    dispatcher::get_default(|dispatch| {
        let id = span.id().expect("Missing ID; this is a bug");
        if let Some(current) = dispatch.current_span().id() {
            dispatch.record_follows_from(&id, current)
        }
    });
}

pub(crate) mod private {

    #[allow(unused)]
    pub trait Sealed {}
}

#[cfg(test)]
#[cfg(feature = "tls")]
pub(crate) mod fixtures {

    use rustls::ServerConfig;

    pub(crate) fn tls_server_config() -> rustls::ServerConfig {
        let (_, cert) = pem_rfc7468::decode_vec(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/minica/example.com/cert.pem"
        )))
        .unwrap();
        let (label, key) = pem_rfc7468::decode_vec(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/minica/example.com/key.pem"
        )))
        .unwrap();

        let cert = rustls::pki_types::CertificateDer::from(cert);
        let key = match label {
            "PRIVATE KEY" => rustls::pki_types::PrivateKeyDer::Pkcs8(key.into()),
            "RSA PRIVATE KEY" => rustls::pki_types::PrivateKeyDer::Pkcs1(key.into()),
            "EC PRIVATE KEY" => rustls::pki_types::PrivateKeyDer::Sec1(key.into()),
            _ => panic!("unknown key type"),
        };

        let mut cfg = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap();

        cfg.alpn_protocols.push(b"h2".to_vec());
        cfg.alpn_protocols.push(b"http/1.1".to_vec());

        cfg
    }

    fn tls_root_store() -> rustls::RootCertStore {
        let mut root_store = rustls::RootCertStore::empty();
        let (_, cert) = pem_rfc7468::decode_vec(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/minica/minica.pem"
        )))
        .unwrap();
        root_store
            .add(rustls::pki_types::CertificateDer::from(cert))
            .unwrap();
        root_store
    }

    pub(crate) fn tls_client_config() -> rustls::ClientConfig {
        let mut config = rustls::ClientConfig::builder()
            .with_root_certificates(tls_root_store())
            .with_no_client_auth();
        config.alpn_protocols.push(b"h2".to_vec());
        config.alpn_protocols.push(b"http/1.1".to_vec());
        config
    }
}
