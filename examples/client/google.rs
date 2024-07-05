//! A simple example of using the `hyperdriver` crate to make a request to a website.

use http::Uri;
use http_body_util::BodyExt as _;
use hyperdriver::client::Client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    tracing_subscriber::fmt::init();

    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut client = Client::build_tcp_http().build();

    let uri: Uri = "https://www.google.com".parse()?;
    let res = client.get(uri.clone()).await?;

    println!("1st Response: {} - {:?}", res.status(), res.version());

    for (name, value) in res.headers() {
        if let Ok(value) = value.to_str() {
            println!("  {}: {}", name, value);
        }
    }

    let r2 = client.get(uri.clone()).await?;
    println!("2nd Response: {}", r2.status());

    let mut body = res.into_body();

    let mut total = 0usize;
    while let Some(Ok(frame)) = body.frame().await {
        if let Some(data) = frame.data_ref() {
            total += data.len();
        }
    }
    println!("Recieved {} body bytes", total);
    drop(r2);

    let r3 = client.get(uri).await?;
    println!("3rd Response: {}", r3.status());

    Ok(())
}
