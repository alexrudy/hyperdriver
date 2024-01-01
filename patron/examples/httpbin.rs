use clap::arg;
use http::{HeaderName, HeaderValue, Uri};
use http_body_util::BodyExt as _;
use patron::Client;
use tokio::io::AsyncWriteExt as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args = clap::Command::new("httpbin")
        .args([
            arg!(-X --method [METHOD] "HTTP method to use").default_value("GET"),
            arg!(-d --body [BODY] "HTTP body to send"),
            arg!(-H --header [HEADER]... "HTTP headers to send"),
        ])
        .get_matches();

    let client = Client::new();

    let uri: Uri = "https://www.httpbin.org/".parse()?;

    let method = args.get_one::<String>("method").unwrap().as_str();
    let method: http::Method = method.parse()?;
    let uri = {
        let mut parts = uri.into_parts();
        parts.path_and_query = Some(format!("/{}", method.as_str().to_lowercase()).parse()?);
        Uri::from_parts(parts)?
    };

    let body = if let Some(body) = args.get_one::<String>("body") {
        arnold::Body::from(body.to_owned())
    } else {
        arnold::Body::empty()
    };

    let mut req = http::Request::builder()
        .method(method)
        .uri(uri)
        .body(body)?;

    let hdrs = req.headers_mut();
    hdrs.append(http::header::USER_AGENT, "patron/0.1.0".parse()?);

    if let Some(headers) = args.get_many::<String>("header") {
        for header in headers {
            let mut parts = header.splitn(2, ':');
            let name: HeaderName = parts.next().unwrap().trim().parse()?;
            let value: HeaderValue = parts.next().unwrap().trim().parse()?;
            hdrs.append(name, value);
        }
    }

    println!("Request: {} {}", req.method(), req.uri());
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
            stdout.write_all(&chunk).await?;
        }
    }

    Ok(())
}
