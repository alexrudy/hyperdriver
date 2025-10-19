//! Test using a custom body type for hyperdriver

use std::{error, fmt, pin::pin};

use bytes::Bytes;
use chateau::client::conn::transport::duplex::DuplexTransport;
use futures_util::StreamExt as _;
use http_body::Body;
use hyperdriver::bridge::io::TokioIo;
use hyperdriver::client::conn::protocol::auto::AlpnHttpConnectionBuilder;
use tower::ServiceExt;

#[derive(Debug)]
struct CustomError {
    message: String,
}

impl fmt::Display for CustomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error: {}", self.message)
    }
}

impl error::Error for CustomError {}

#[derive(Debug, Default)]
struct CustomBody {
    data: String,
}

impl From<hyper::body::Incoming> for CustomBody {
    fn from(_value: hyper::body::Incoming) -> Self {
        Default::default()
    }
}

impl Body for CustomBody {
    type Data = Bytes;

    type Error = CustomError;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        std::task::Poll::Ready(Some(Ok(http_body::Frame::data(self.data.clone().into()))))
    }
}

#[tokio::test]
async fn client() {
    let _ = tracing_subscriber::fmt::try_init();

    let (tx, incoming) = chateau::stream::duplex::pair();

    let acceptor: hyperdriver::server::conn::Acceptor =
        hyperdriver::server::conn::Acceptor::from(incoming);

    let jh = tokio::task::spawn(serve_one_h1(acceptor));

    let client = hyperdriver::client::Client::builder()
        .with_protocol(AlpnHttpConnectionBuilder::default())
        .with_transport(DuplexTransport::new(1024, tx))
        .with_default_pool()
        .with_body::<CustomBody, CustomBody>()
        .build_service();

    let res = client
        .oneshot(
            http::Request::get("http://test")
                .body(CustomBody::default())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), http::StatusCode::OK);
    jh.await.unwrap().unwrap();
}

async fn service_ok(
    req: http::Request<hyper::body::Incoming>,
) -> Result<http::Response<hyperdriver::Body>, Box<dyn error::Error + Send + Sync>> {
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

async fn serve_one_h1(
    acceptor: hyperdriver::server::conn::Acceptor,
) -> Result<(), Box<dyn error::Error + Send + Sync>> {
    let mut acceptor: std::pin::Pin<&mut hyperdriver::server::conn::Acceptor> = pin!(acceptor);
    let stream = acceptor.next().await.ok_or("no connection")??;

    let service = hyper::service::service_fn(service_ok);

    let conn =
        hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service);

    conn.await?;

    Ok(())
}
