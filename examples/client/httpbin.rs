//! A simple HTTP client that sends a request to httpbin.org and prints the response.
//!
//! Defaults to sending a GET request to https://www.httpbin.org/ over HTTP/1.1.
//!
//! Run with `--help` to see options.
use clap::arg;
use http::{HeaderName, HeaderValue, Uri};
use http_body_util::BodyExt as _;
use hyperdriver::client::Client;
use tokio::io::AsyncWriteExt as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    tracing_subscriber::fmt::init();

    let _ = rustls::crypto::ring::default_provider().install_default();

    let args = clap::Command::new("httpbin")
        .args([
            arg!(-X --method [METHOD] "HTTP method to use").default_value("GET"),
            arg!(-d --body [BODY] "HTTP body to send"),
            arg!(-H --header [HEADER]... "HTTP headers to send"),
            arg!(--http2 "Use HTTP/2"),
        ])
        .get_matches();

    let mut client = Client::build_tcp_http().build();

    let uri: Uri = "https://www.httpbin.org/".parse()?;

    let method = args.get_one::<String>("method").unwrap().as_str();
    let method: http::Method = method.parse()?;
    let uri = {
        let mut parts = uri.into_parts();
        parts.path_and_query = Some(format!("/{}", method.as_str().to_lowercase()).parse()?);
        Uri::from_parts(parts)?
    };

    let body = if let Some(body) = args.get_one::<String>("body") {
        hyperdriver::body::Body::from(body.to_owned())
    } else {
        hyperdriver::body::Body::empty()
    };

    let mut req = http::Request::builder()
        .method(method)
        .uri(uri)
        .body(body)?;

    if args.get_flag("http2") {
        *req.version_mut() = http::Version::HTTP_2;
    }

    let hdrs = req.headers_mut();
    hdrs.append(
        http::header::USER_AGENT,
        concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")).parse()?,
    );

    if let Some(headers) = args.get_many::<String>("header") {
        for header in headers {
            let mut parts = header.splitn(2, ':');
            let name: HeaderName = parts.next().unwrap().trim().parse()?;
            let value: HeaderValue = parts.next().unwrap().trim().parse()?;
            hdrs.append(name, value);
        }
    }

    println!(
        "Request: {} {} {:?}",
        req.method(),
        req.uri(),
        req.version()
    );
    for (name, value) in req.headers() {
        if let Ok(value) = value.to_str() {
            println!("  {}: {}", name, value);
        }
    }

    let res = client.request(req).await?;

    println!("Response: {} - {:?}", res.status(), res.version());

    for (name, value) in res.headers() {
        if let Ok(value) = value.to_str() {
            println!("  {}: {}", name, value);
        }
    }

    let mut stdout = tokio::io::stdout();

    let mut body = res.into_body();
    while let Some(chunk) = body.frame().await {
        if let Some(chunk) = chunk?.data_ref() {
            stdout.write_all(chunk).await?;
        }
    }

    Ok(())
}
