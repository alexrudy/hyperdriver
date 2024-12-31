//! Test hyperdriver client using low-level connector.
use futures_util::StreamExt;
use http::StatusCode;
use hyperdriver::bridge::io::TokioIo;
use hyperdriver::client::conn::connector::ConnectorLayer;
use hyperdriver::client::conn::Connection as _;
use hyperdriver::service::RequestExecutor;
use hyperdriver::Body;
use std::pin::pin;
use tower::ServiceExt;

use hyperdriver::client::conn::protocol::http1::Builder as HttpConnectionBuilder;
use hyperdriver::client::conn::transport::duplex::DuplexTransport;
type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[tokio::test]
async fn client() -> Result<(), BoxError> {
    let _ = tracing_subscriber::fmt::try_init();

    let (tx, incoming) = hyperdriver::stream::duplex::pair();

    let acceptor: hyperdriver::server::conn::Acceptor =
        hyperdriver::server::conn::Acceptor::from(incoming);

    let server = tokio::spawn(serve_one_h1(acceptor));

    let client = tower::ServiceBuilder::new()
        .layer(ConnectorLayer::new(
            DuplexTransport::new(1024, tx.clone()),
            HttpConnectionBuilder::new(),
        ))
        .service(RequestExecutor::new());

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

    let (tx, incoming) = hyperdriver::stream::duplex::pair();

    let acceptor: hyperdriver::server::conn::Acceptor =
        hyperdriver::server::conn::Acceptor::from(incoming);

    let server = tokio::spawn(serve_one_h1(acceptor));

    let req = http::Request::get("http://test/").body(Body::empty())?;
    let (parts, body) = req.into_parts();

    let connector = hyperdriver::client::conn::Connector::new(
        DuplexTransport::new(1024, tx.clone()),
        HttpConnectionBuilder::new(),
        parts.clone(),
        http::Version::HTTP_11.into(),
    );

    let mut conn = connector.await?;
    let resp = conn
        .send_request(http::Request::from_parts(parts, body))
        .await?;

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
