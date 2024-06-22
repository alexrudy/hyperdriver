//! An echoing HTTP/2 server with TLS.
//!
//! The TLS is set up with `minica`, a minimal CA for testing purposes.
//!
//! `minica` is set to use `example.com` as the domain, so clients should
//! connect with that domain name.

use std::convert::Infallible;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;

use hyper::server::conn::http2;
use hyperdriver::bridge::rt::TokioExecutor;
use hyperdriver::server::conn::tls::TlsConnectionInfoLayer;
use hyperdriver::server::conn::MakeServiceConnectionInfoLayer;
use tower::make::Shared;
use tower::Layer;
use tracing_subscriber::EnvFilter;

fn tls_config(domain: &str) -> rustls::ServerConfig {
    let cert_data = std::fs::read(format!("examples/server/minica/{domain}/cert.pem")).unwrap();
    let (_, cert) = pem_rfc7468::decode_vec(&cert_data).unwrap();

    let key_data = std::fs::read(format!("examples/server/minica/{domain}/key.pem")).unwrap();
    let (label, key) = pem_rfc7468::decode_vec(&key_data).unwrap();

    let cert = rustls::pki_types::CertificateDer::from(cert);
    let key = match label {
        "PRIVATE KEY" => rustls::pki_types::PrivateKeyDer::Pkcs8(key.into()),
        "RSA PRIVATE KEY" => rustls::pki_types::PrivateKeyDer::Pkcs1(key.into()),
        "EC PRIVATE KEY" => rustls::pki_types::PrivateKeyDer::Sec1(key.into()),
        _ => panic!("unknown key type"),
    };

    rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap()
}

#[tokio::main]
async fn main() {
    use http_body_util::BodyExt;

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let addr = SocketAddr::V4(SocketAddrV4::new([127, 0, 0, 1].into(), 0));
    let incoming = tokio::net::TcpListener::bind(addr).await.unwrap();
    let addr = incoming.local_addr().unwrap();

    let acceptor = hyperdriver::server::conn::Acceptor::from(incoming)
        .with_tls(Arc::new(tls_config("localhost")));

    let svc = tower::service_fn(|req: hyperdriver::body::Request| async {
        let body = req.into_body();
        let data = body.collect().await.unwrap();
        Ok::<_, Infallible>(hyperdriver::body::Response::new(
            hyperdriver::body::Body::from(data.to_bytes()),
        ))
    });

    let server = hyperdriver::server::Server::new(
        acceptor,
        TlsConnectionInfoLayer::new()
            .layer(MakeServiceConnectionInfoLayer::new().layer(Shared::new(svc))),
    )
    .with_protocol(http2::Builder::new(TokioExecutor::new()));

    let (tx, rx) = tokio::sync::oneshot::channel();

    let handle = tokio::spawn(async move {
        server
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await
    });
    println!("Server listening on {}", addr);

    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!();
        println!("ctrl-c received, shutting down");
        tx.send(()).unwrap();
    });

    handle.await.unwrap().unwrap();
}
