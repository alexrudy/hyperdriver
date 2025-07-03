//! Test hyperdriver client using unix stream transports.
use std::pin::pin;

use camino::Utf8PathBuf;
use futures_util::StreamExt as _;
use http::StatusCode;
use hyperdriver::bridge::io::TokioIo;
use hyperdriver::client::conn::transport::unix::{StaticAddressUnixTransport, UnixTransport};
use hyperdriver::server::conn::Acceptor;
use hyperdriver::stream::unix::{UnixAddr, UnixListener};

use hyperdriver::stream::UnixStream;
use hyperdriver::{Body, Client};

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

const SOCKET: &str = "test-transport.sock";

#[tokio::test]
async fn transport() -> Result<(), BoxError> {
    let _ = tracing_subscriber::fmt::try_init();

    let dir = tempfile::tempdir().unwrap();

    let listener = UnixListener::bind(dir.path().join(SOCKET)).unwrap();
    let acceptor: Acceptor<UnixListener> = Acceptor::new(listener);
    let server = tokio::spawn(serve_one_h1(acceptor));

    let transport: UnixTransport<UnixStream> = UnixTransport::new(Default::default());
    let mut client = Client::builder()
        .without_pool()
        .with_transport(transport)
        .with_auto_http()
        .without_redirects()
        .without_tls()
        .build();

    let addr = UnixAddr::from_pathbuf(dir.path().join(SOCKET).try_into().unwrap());
    let req = http::Request::get("http://test/")
        .extension(addr)
        .body(Body::empty())?;
    let resp: http::Response<Body> = client.request(req).await?.map(Into::into);

    assert_eq!(resp.status(), StatusCode::OK);
    server.abort();
    let _ = server.await;

    Ok(())
}

#[tokio::test]
async fn static_address_transport() -> Result<(), BoxError> {
    let _ = tracing_subscriber::fmt::try_init();

    let dir = tempfile::tempdir().unwrap();

    let listener = UnixListener::bind(dir.path().join(SOCKET)).unwrap();
    let acceptor: Acceptor<UnixListener> = Acceptor::new(listener);
    let server = tokio::spawn(serve_one_h1(acceptor));

    let path: Utf8PathBuf = dir.path().join(SOCKET).try_into().unwrap();
    let transport: StaticAddressUnixTransport<UnixStream> = StaticAddressUnixTransport::new(path);
    let mut client = Client::builder()
        .without_pool()
        .with_transport(transport)
        .with_auto_http()
        .without_redirects()
        .without_tls()
        .build();

    let req = http::Request::get("http://test/").body(Body::empty())?;
    let resp: http::Response<Body> = client.request(req).await?.map(Into::into);

    assert_eq!(resp.status(), StatusCode::OK);
    server.abort();
    let _ = server.await;

    Ok(())
}

async fn service_ok(
    req: http::Request<hyper::body::Incoming>,
) -> Result<http::Response<hyperdriver::Body>, BoxError> {
    Ok(http::response::Builder::new()
        .status(200)
        .header(
            "O-Host",
            req.headers()
                .get("Host")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("missing"),
        )
        .body(hyperdriver::body::Body::empty())?)
}

async fn serve_one_h1(acceptor: Acceptor<UnixListener>) -> Result<(), BoxError> {
    let mut acceptor = pin!(acceptor);
    let stream = acceptor.next().await.ok_or("no connection")??;

    let service = hyper::service::service_fn(service_ok);

    let conn =
        hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service);

    conn.await?;

    Ok(())
}
