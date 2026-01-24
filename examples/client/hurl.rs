//! A simple HTTP client that uses the hyperdriver crate.
//!
//! This client can send HTTP requests to a server and print the response.
//! It provides a simple command-line interface, use `--help` to see the options.

use clap::{ArgAction, arg, value_parser};
use http::Uri;
use http_body_util::BodyExt as _;
use hyperdriver::client::Client;
use tokio::io::AsyncWriteExt;
use tracing::Level;
use tracing_subscriber::{
    Layer as _, filter::Targets, fmt::format::FmtSpan, layer::SubscriberExt as _,
    util::SubscriberInitExt as _,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    let filter = Targets::new()
        .with_target("hyperdriver", Level::TRACE)
        .with_target("hello", Level::TRACE)
        .with_default(Level::INFO);

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_span_events(FmtSpan::CLOSE)
                .with_filter(filter.clone()),
        )
        .init();

    let _ = rustls::crypto::ring::default_provider().install_default();

    let args = clap::Command::new("hurl")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Hyperdriver HTTP client")
        .args([
            clap::Arg::new("uri").help("Target URI").required(true),
            arg!(-X --method [METHOD] "HTTP method to use").default_value("GET"),
            arg!(-d --body [BODY] "HTTP body to send"),
            arg!(-H --header [HEADER]... "HTTP headers to send"),
            arg!(--timeout [SECONDS] "Timeout for the request in seconds")
                .default_value("30")
                .value_parser(value_parser!(u64).range(1..)),
            clap::Arg::new("http2")
                .long("http2")
                .short('2')
                .help("Use HTTP/2")
                .action(ArgAction::SetTrue),
            clap::Arg::new("http1")
                .long("http1")
                .short('1')
                .help("Use HTTP/1")
                .action(ArgAction::SetTrue)
                .conflicts_with("http2"),
            arg!(--tls [CERTIFICATE] "Path to a CA root for TLS"),
        ])
        .get_matches();

    let mut client = Client::builder()
        .with_tcp(Default::default())
        .with_auto_http();

    if let Some(tls_root) = args.get_one::<String>("tls") {
        println!("Using TLS root: {tls_root}");

        let config = rustls::ClientConfig::builder();
        let mut roots = rustls::RootCertStore::empty();
        let (_, cert) = pem_rfc7468::decode_vec(&std::fs::read(tls_root).unwrap()).unwrap();
        roots
            .add(rustls::pki_types::CertificateDer::from(cert))
            .unwrap();
        let mut config = config.with_root_certificates(roots).with_no_client_auth();
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        client = client.with_tls(config);
    }

    let method: http::Method = {
        let method = args.get_one::<String>("method").unwrap().as_str();
        method.parse()?
    };

    let client = client.build();

    let uri: Uri = args
        .get_one::<String>("uri")
        .expect("uri argument must be present")
        .parse()?;

    let version = if args.get_flag("http1") {
        http::Version::HTTP_11
    } else if args.get_flag("http2") {
        http::Version::HTTP_2
    } else {
        match uri.scheme_str() {
            Some("https") => http::Version::HTTP_2,
            _ => http::Version::HTTP_11,
        }
    };

    let mut hdrs = http::HeaderMap::new();
    hdrs.append(
        http::header::USER_AGENT,
        concat!(
            "hurl ",
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        )
        .parse()?,
    );

    if let Some(headers) = args.get_many::<String>("header") {
        for header in headers {
            let mut parts = header.splitn(2, ':');
            let name: http::HeaderName = parts.next().unwrap().trim().parse()?;
            let value: http::HeaderValue = parts.next().unwrap().trim().parse()?;
            hdrs.append(name, value);
        }
    }

    send(
        client.clone(),
        uri.clone(),
        version,
        method.clone(),
        hdrs.clone(),
    )
    .await;

    Ok(())
}

#[tracing::instrument(level = "trace", skip_all)]
async fn send(
    mut client: Client,
    uri: Uri,
    version: http::Version,
    method: http::Method,
    headers: http::HeaderMap,
) {
    let mut builder = http::Request::builder()
        .uri(uri.clone())
        .method(method)
        .version(version);

    *builder.headers_mut().unwrap() = headers;

    let req = builder.body(hyperdriver::body::Body::empty()).unwrap();

    let res = match client.request(req).await {
        Ok(res) => res,
        Err(err) => {
            println!("Error: {err}");
            return;
        }
    };

    let (parts, mut body) = res.into_parts();
    let mut stdout = tokio::io::stdout();
    let mut total = 0usize;
    while let Some(Ok(frame)) = body.frame().await {
        if let Some(data) = frame.data_ref() {
            total += data.len();
            stdout.write_all(data).await.unwrap();
        }
    }

    let res = http::Response::from_parts(parts, ());

    println!("Response: {} - {:?} {total}", res.status(), res.version());
}
