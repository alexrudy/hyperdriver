use std::{net::Ipv4Addr, sync::Arc};

use rustls::ServerConfig;

fn tls_config() -> rustls::ServerConfig {
    let (_, cert) = pem_rfc7468::decode_vec(include_bytes!("minica/example.com/cert.pem")).unwrap();
    let (_, key) = pem_rfc7468::decode_vec(include_bytes!("minica/example.com/key.pem")).unwrap();

    let cert = rustls::Certificate(cert);
    let key = rustls::PrivateKey(key);

    ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap()
}

fn tls_root_store() -> rustls::RootCertStore {
    let mut root_store = rustls::RootCertStore::empty();
    let (_, cert) = pem_rfc7468::decode_vec(include_bytes!("minica/minica.pem")).unwrap();
    root_store.add(&rustls::Certificate(cert)).unwrap();
    root_store
}

#[tokio::test]
async fn braided_tls() {
    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let incoming = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = incoming.local_addr().unwrap();

    let server = braid::server::acceptor::Acceptor::from(incoming).tls(Arc::new(tls_config()));

    tokio::spawn(async move {
        let mut incoming = server.fuse();
        while let Some(Ok(mut stream)) = incoming.next().await {
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).await.unwrap();
            stream.write_all(&buf[..n]).await.unwrap();
        }
    });

    let mut conn = braid::client::Stream::connect(addr)
        .await
        .unwrap()
        .tls("example.com".try_into().unwrap(), tls_root_store());

    let mut buf = [0u8; 1024];
    conn.write_all(b"hello world").await.unwrap();
    let n = conn.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"hello world");
}
