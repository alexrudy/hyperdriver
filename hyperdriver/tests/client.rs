use futures_util::StreamExt;
use http::StatusCode;
use hyperdriver::bridge::io::TokioIo;
use std::pin::pin;

use hyperdriver::client::conn::duplex::DuplexTransport;
use hyperdriver::client::{HttpConnectionBuilder, PoolConfig};
type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[tokio::test]
async fn client() -> Result<(), BoxError> {
    let (client, incoming) = hyperdriver::stream::duplex::pair("test".parse().unwrap());

    let acceptor = hyperdriver::stream::server::Acceptor::from(incoming);

    tokio::spawn(serve_one_h1(acceptor));

    let mut client = hyperdriver::client::Client::new(
        HttpConnectionBuilder::default(),
        DuplexTransport::new(1024, None, client),
        PoolConfig::default(),
    );

    let resp = client.get("http://test/".parse().unwrap()).await?;

    assert_eq!(resp.status(), StatusCode::OK);

    Ok(())
}

async fn service_ok(
    req: http::Request<hyper::body::Incoming>,
) -> Result<hyperdriver::body::Response, BoxError> {
    Ok(http::response::Builder::new()
        .status(200)
        .header("O-Host", req.headers().get("Host").unwrap())
        .body(hyperdriver::body::Body::empty())?)
}

async fn serve_one_h1(acceptor: hyperdriver::stream::server::Acceptor) -> Result<(), BoxError> {
    let mut acceptor = pin!(acceptor);
    let stream = acceptor.next().await.ok_or("no connection")??;

    let service = hyper::service::service_fn(service_ok);

    let conn =
        hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service);

    conn.await?;

    Ok(())
}
