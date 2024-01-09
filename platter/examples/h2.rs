use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;

use hyper::body::Incoming;
use hyper::server::conn::http2;
use hyper_util::rt::TokioExecutor;

fn tls_config(domain: &str) -> rustls::ServerConfig {
    let cert_data = std::fs::read(format!("minica/{domain}/cert.pem")).unwrap();
    let (_, cert) = pem_rfc7468::decode_vec(&cert_data).unwrap();

    let key_data = std::fs::read(format!("minica/{domain}/key.pem")).unwrap();
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

    tracing_subscriber::fmt::init();

    let addr = SocketAddr::V4(SocketAddrV4::new([127, 0, 0, 1].into(), 0));
    let incoming = tokio::net::TcpListener::bind(addr).await.unwrap();
    let addr = incoming.local_addr().unwrap();

    let acceptor = braid::server::Acceptor::from(incoming).tls(Arc::new(tls_config("localhost")));

    let server = platter::Server::new(
        acceptor,
        tower::service_fn(|_| async {
            Ok::<_, hyper::Error>(tower::service_fn(|req: http::Request<Incoming>| async {
                let body = req.into_body();
                let data = body.collect().await?;
                Ok::<_, hyper::Error>(hyper::Response::new(arnold::Body::from(data.to_bytes())))
            }))
        }),
    )
    .with_protocol(http2::Builder::new(TokioExecutor::new()));

    let (tx, rx) = tokio::sync::oneshot::channel();

    let handle = tokio::spawn(async move {
        tokio::pin!(server);

        tokio::select! {
            rv = &mut server => {
                return rv;
            },
            _ = rx => {
                return Ok(());
            },
        }
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
