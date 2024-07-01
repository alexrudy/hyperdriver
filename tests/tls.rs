use std::future::Future;

use http::Request;
use http::Response;
use http_body::Body as HttpBody;
use http_body_util::BodyExt as _;
use hyperdriver::bridge::rt::TokioExecutor;
use hyperdriver::client::conn::transport::TransportExt as _;
use hyperdriver::server::conn::Accept;
use hyperdriver::service::MakeServiceRef;
use rustls::ServerConfig;

use hyperdriver::server::{Protocol, Server};
type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

fn tls_config() -> rustls::ServerConfig {
    let (_, cert) = pem_rfc7468::decode_vec(include_bytes!("minica/example.com/cert.pem")).unwrap();
    let (label, key) =
        pem_rfc7468::decode_vec(include_bytes!("minica/example.com/key.pem")).unwrap();

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
    let (_, cert) = pem_rfc7468::decode_vec(include_bytes!("minica/minica.pem")).unwrap();
    root_store
        .add(rustls::pki_types::CertificateDer::from(cert))
        .unwrap();
    root_store
}

async fn echo(req: hyperdriver::body::Request) -> Result<hyperdriver::body::Response, BoxError> {
    tracing::trace!("processing request");
    let body = req.into_body();
    let data = body.collect().await?;
    tracing::trace!("collected body, responding");
    Ok(Response::new(hyperdriver::body::Body::from(
        data.to_bytes(),
    )))
}

fn serve_gracefully<A, P, S, B>(
    server: Server<A, P, S, B>,
) -> impl Future<Output = Result<(), BoxError>>
where
    S: MakeServiceRef<A::Conn, B> + Send + 'static,
    S::Future: Send + 'static,
    P: Protocol<S::Service, A::Conn> + Send + 'static,
    A: Accept + Unpin + Send + 'static,
    B: HttpBody + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(server.with_graceful_shutdown(async {
        if rx.await.is_err() {
            tracing::trace!("shutdown with err?");
        }
    }));
    tracing::trace!("spawned server");

    async move {
        tracing::trace!("sending shutdown signal");
        let _ = tx.send(());
        handle.await.unwrap().map_err(Into::into)
    }
}

#[tokio::test]
async fn tls_echo_h1() {
    use hyper::client::conn::http1::Builder;
    use hyperdriver::client::conn::transport::duplex::DuplexTransport;
    use hyperdriver::Client;

    let _ = tracing_subscriber::fmt::try_init();

    let (duplex_client, incoming) = hyperdriver::stream::duplex::pair();

    let acceptor =
        hyperdriver::server::conn::Acceptor::from(incoming).with_tls(tls_config().into());

    let server = hyperdriver::server::Server::builder()
        .with_acceptor(acceptor)
        .with_shared_service(tower::service_fn(echo))
        .with_http1();

    let handle = serve_gracefully(server);

    let mut client_tls = rustls::ClientConfig::builder()
        .with_root_certificates(tls_root_store())
        .with_no_client_auth();
    client_tls.alpn_protocols.push(b"http/1.1".to_vec());

    let mut client = Client::builder()
        .with_protocol(Builder::new())
        .with_default_pool()
        .with_transport(DuplexTransport::new(1024, duplex_client))
        .with_tls(client_tls)
        .build();

    let response: Response<hyperdriver::Body> = client
        .request(
            http::Request::builder()
                .uri("https://example.com/")
                .version(http::Version::HTTP_11)
                .body(hyperdriver::body::Body::from("hello world"))
                .unwrap(),
        )
        .await
        .unwrap();
    tracing::trace!("sent request");
    let (_, body) = response.into_parts();

    let data = body.collect().await.unwrap().to_bytes();
    assert_eq!(&*data, b"hello world");

    handle.await.unwrap();
}

#[tokio::test]
async fn tls_echo_h2() {
    use hyper::client::conn::http2::Builder;
    use hyperdriver::client::conn::transport::duplex::DuplexTransport;
    use hyperdriver::Client;

    let _ = tracing_subscriber::fmt::try_init();

    let (duplex_client, incoming) = hyperdriver::stream::duplex::pair();

    let acceptor =
        hyperdriver::server::conn::Acceptor::from(incoming).with_tls(tls_config().into());
    let server = hyperdriver::server::Server::builder()
        .with_acceptor(acceptor)
        .with_shared_service(tower::service_fn(echo))
        .with_http2();

    let guard = serve_gracefully(server);

    let mut client_tls = rustls::ClientConfig::builder()
        .with_root_certificates(tls_root_store())
        .with_no_client_auth();
    client_tls.alpn_protocols.push(b"h2".to_vec());

    let mut client = Client::builder()
        .with_protocol(Builder::new(TokioExecutor::new()))
        .with_transport(DuplexTransport::new(1024, duplex_client).with_tls(client_tls.into()))
        .build();

    let response: Response<hyperdriver::Body> = client
        .request(
            Request::builder()
                .uri("https://example.com/")
                .version(http::Version::HTTP_2)
                .body("Hello H2".into())
                .unwrap(),
        )
        .await
        .unwrap();
    let (_, body) = response.into_parts();

    let data = body.collect().await.unwrap().to_bytes();
    assert_eq!(&*data, b"Hello H2");

    guard.await.unwrap();
}
