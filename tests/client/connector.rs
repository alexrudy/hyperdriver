//! Test hyperdriver client using low-level connector.
use chateau::client::conn::connector::ConnectorLayer;
use futures_util::StreamExt;
use http::StatusCode;
use hyperdriver::{bridge::io::TokioIo, client::conn::protocol::Http1Builder};

use chateau::client::conn::service::ClientExecutorService;
use chateau::client::conn::Connection as _;
use hyperdriver::Body;
use std::pin::pin;
use tower::ServiceExt;

use chateau::client::conn::transport::duplex::DuplexTransport;
use hyperdriver::client::conn::protocol::Http1Builder as HttpConnectionBuilder;
type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[tokio::test]
async fn client() -> Result<(), BoxError> {
    let _ = tracing_subscriber::fmt::try_init();

    let (tx, incoming) = chateau::stream::duplex::pair();

    let acceptor: hyperdriver::server::conn::Acceptor =
        hyperdriver::server::conn::Acceptor::from(incoming);

    let server = tokio::spawn(serve_one_h1(acceptor));

    let client = tower::ServiceBuilder::new()
        .layer(ConnectorLayer::new(
            DuplexTransport::new(1024, tx.clone()),
            HttpConnectionBuilder::default(),
        ))
        .service(ClientExecutorService::new());

    let req = http::Request::get("http://test/").body(Body::empty())?;

    let resp: http::Response<Body> = client.oneshot(req).await?.map(Into::into);

    assert_eq!(resp.status(), StatusCode::OK);
    server.abort();
    let _ = server.await;

    Ok(())
}

#[tokio::test]
async fn connector() -> Result<(), BoxError> {
    let _ = tracing_subscriber::fmt::try_init();

    let (tx, incoming) = chateau::stream::duplex::pair();

    let acceptor: hyperdriver::server::conn::Acceptor =
        hyperdriver::server::conn::Acceptor::from(incoming);

    let server = tokio::spawn(serve_one_h1(acceptor));

    let req = http::Request::get("http://test/").body(Body::empty())?;

    let connector = chateau::client::conn::Connector::new(
        DuplexTransport::new(1024, tx.clone()),
        Http1Builder::default(),
        req,
    );

    let (mut conn, req) = connector.await?;
    let resp = conn.send_request(req).await?;

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

async fn serve_one_h1(acceptor: hyperdriver::server::conn::Acceptor) -> Result<(), BoxError> {
    let mut acceptor = pin!(acceptor);
    let stream = acceptor.next().await.ok_or("no connection")??;

    let service = hyper::service::service_fn(service_ok);

    let conn =
        hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service);

    conn.await?;

    Ok(())
}
