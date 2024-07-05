use futures_util::StreamExt;
use http::StatusCode;
use hyperdriver::body::Response;
use hyperdriver::bridge::io::TokioIo;
use hyperdriver::bridge::rt::TokioExecutor;
use std::pin::pin;

use hyperdriver::client::conn::protocol::auto::HttpConnectionBuilder;
use hyperdriver::client::conn::transport::duplex::DuplexTransport;
type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[tokio::test]
async fn client() -> Result<(), BoxError> {
    let (tx, incoming) = hyperdriver::stream::duplex::pair();

    let acceptor: hyperdriver::server::conn::Acceptor =
        hyperdriver::server::conn::Acceptor::from(incoming);

    let server = tokio::spawn(serve_one_h1(acceptor));

    let mut client = hyperdriver::client::Client::builder()
        .with_protocol(HttpConnectionBuilder::default())
        .with_transport(DuplexTransport::new(1024, tx.clone()))
        .with_default_pool()
        .build();

    let resp: Response = client.get("http://test/".parse().unwrap()).await?;

    assert_eq!(resp.status(), StatusCode::OK);
    server.abort();
    let _ = server.await;

    Ok(())
}

#[tokio::test]
async fn client_h2() -> Result<(), BoxError> {
    let (tx, incoming) = hyperdriver::stream::duplex::pair();

    let acceptor: hyperdriver::server::conn::Acceptor =
        hyperdriver::server::conn::Acceptor::from(incoming);

    let server = tokio::spawn(serve_one_h2(acceptor));

    let mut client = hyperdriver::client::Client::builder()
        .with_protocol(HttpConnectionBuilder::default())
        .with_transport(DuplexTransport::new(1024, tx))
        .with_default_pool()
        .build();

    let request = http::Request::get("http://test/")
        .version(http::Version::HTTP_2)
        .body(hyperdriver::body::Body::empty())?;
    let resp: Response = client.request(request).await?;

    assert_eq!(resp.status(), StatusCode::OK);

    server.abort();
    let _ = server.await;

    Ok(())
}

async fn service_ok(
    req: http::Request<hyper::body::Incoming>,
) -> Result<hyperdriver::body::Response, BoxError> {
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

async fn serve_one_h1(acceptor: hyperdriver::server::conn::Acceptor) -> Result<(), BoxError> {
    let mut acceptor = pin!(acceptor);
    let stream = acceptor.next().await.ok_or("no connection")??;

    let service = hyper::service::service_fn(service_ok);

    let conn =
        hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service);

    conn.await?;

    Ok(())
}

async fn serve_one_h2(acceptor: hyperdriver::server::conn::Acceptor) -> Result<(), BoxError> {
    let mut acceptor = pin!(acceptor);
    let stream = acceptor.next().await.ok_or("no connection")??;

    let service = hyper::service::service_fn(service_ok);

    let conn = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(stream), service);

    conn.await?;

    Ok(())
}
