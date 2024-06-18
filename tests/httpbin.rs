use http_body_util::BodyExt as _;
use hyperdriver::client::{conn::TcpTransportConfig, Client};

/// Make a request to httpbin.org
async fn httpbin_request(
    mut req: http::Request<hyperdriver::Body>,
) -> Result<bytes::Bytes, Box<dyn std::error::Error + Send + Sync + 'static>> {
    let config = TcpTransportConfig {
        happy_eyeballs_timeout: Some(std::time::Duration::from_secs(1)),
        ..Default::default()
    };

    let client = Client::builder()
        .with_tcp(config)
        .with_default_tls()
        .build();

    let hdrs = req.headers_mut();
    hdrs.append(
        http::header::USER_AGENT,
        concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")).parse()?,
    );

    let res = client.request(req).await?;

    let body = res.into_body().collect().await?.to_bytes();
    Ok(body)
}

fn request(method: http::Method, version: http::Version) -> http::request::Builder {
    http::Request::builder()
        .uri(format!("https://httpbin.org/{}", method.as_str()))
        .method(method)
        .version(version)
}

#[tokio::test]
async fn get_h1() {
    let req = request(http::Method::GET, http::Version::HTTP_11)
        .body(hyperdriver::Body::empty())
        .unwrap();
    let body = httpbin_request(req).await.unwrap();
    assert!(!body.is_empty());
}

#[tokio::test]
async fn get_h2() {
    let req = request(http::Method::GET, http::Version::HTTP_2)
        .body(hyperdriver::Body::empty())
        .unwrap();
    let body = httpbin_request(req).await.unwrap();
    assert!(!body.is_empty());
}

#[tokio::test]
async fn post_h1() {
    let req = request(http::Method::POST, http::Version::HTTP_11)
        .body(hyperdriver::Body::from("Hello, world!"))
        .unwrap();
    let body = httpbin_request(req).await.unwrap();
    assert!(!body.is_empty());
}

#[tokio::test]
async fn post_h2() {
    let req = request(http::Method::POST, http::Version::HTTP_2)
        .body(hyperdriver::Body::from("Hello, world!"))
        .unwrap();
    let body = httpbin_request(req).await.unwrap();
    assert!(!body.is_empty());
}
