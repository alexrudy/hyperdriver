//! A simple HTTP client that sends a request to httpbin.org and prints the response.
//!
//! Defaults to sending a GET request to https://www.httpbin.org/ over HTTP/1.1.
//!
//! Run with `--help` to see options.

use std::time::Duration;

use clap::{arg, value_parser, ArgMatches};
use futures_util::{stream::FuturesUnordered, TryStreamExt};
use http::{HeaderName, HeaderValue, Uri};
use http_body_util::BodyExt as _;
use hyperdriver::{client::Client, Body};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::trace::Tracer;
use opentelemetry_sdk::Resource;
use opentelemetry_semantic_conventions::{
    resource::{DEPLOYMENT_ENVIRONMENT_NAME, SERVICE_NAME, SERVICE_VERSION},
    SCHEMA_URL,
};
use tokio::io::AsyncWriteExt as _;
use tracing::{instrument, Instrument, Level};
use tracing_subscriber::{filter::Targets, fmt::format::FmtSpan, prelude::*};

const NAME: &str = "profile";

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    init_tracing()?;
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args = clap::Command::new("profile-hyperdriver")
        .args([
            arg!(-X --method [METHOD] "HTTP method to use").default_value("GET"),
            arg!(-d --body [BODY] "HTTP body to send"),
            arg!(-H --header [HEADER]... "HTTP headers to send"),
            arg!(-n --repeat [TIMES] "Repeat the request (n times)")
                .default_value("1")
                .value_parser(value_parser!(u64).range(1..)),
            arg!(--timeout [SECONDS] "Timeout for the request in seconds")
                .default_value("30")
                .value_parser(value_parser!(u64).range(1..)),
            arg!(--http2 "Use HTTP/2"),
        ])
        .get_matches();

    demonstrate_requests(&args).await?;

    // Helps to ensure that the telemetry tracer can send data
    tokio::time::sleep(Duration::from_secs(1)).await;

    Ok(())
}

async fn build_request(args: &ArgMatches, uri: Uri) -> Result<http::Request<Body>, BoxError> {
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

    Ok(req)
}

#[instrument(level = "info", skip_all)]
async fn demonstrate_requests(args: &ArgMatches) -> Result<(), BoxError> {
    let timeout = Duration::from_secs(*args.get_one::<u64>("timeout").unwrap());

    let uri: Uri = "https://www.httpbin.org/".parse()?;

    let repeat = *args.get_one::<u64>("repeat").unwrap();

    let req = build_request(args, uri.clone()).await?;
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

    let mut client = Client::build_tcp_http().with_timeout(timeout).build();

    let mut fut = FuturesUnordered::new();
    for _ in 1..=repeat {
        let req = build_request(args, uri.clone()).await?;
        let res = client.request(req);
        let span = tracing::info_span!("request");
        fut.push(res.instrument(span))
    }

    {
        let mut res = None;
        while let Some(item) = fut.try_next().await? {
            res = Some(item);
        }

        let res = res.unwrap();
        println!("Response: {} - {:?}", res.status(), res.version());

        for (name, value) in res.headers() {
            if let Ok(value) = value.to_str() {
                println!("  {}: {}", name, value);
            }
        }

        let span = tracing::debug_span!("read body");
        async {
            let mut stdout = tokio::io::stdout();

            let mut body = res.into_body();
            while let Some(chunk) = body.frame().await {
                if let Some(chunk) = chunk?.data_ref() {
                    stdout.write_all(chunk).await?;
                }
            }
            Ok::<_, BoxError>(())
        }
        .instrument(span)
        .await?;
    }

    tracing::info!("finished");

    Ok(())
}

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

// Create a Resource that captures information about the entity for which telemetry is recorded.
fn resource() -> Resource {
    Resource::builder()
        .with_schema_url(
            [
                KeyValue::new(SERVICE_NAME, env!("CARGO_PKG_NAME")),
                KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
                KeyValue::new(DEPLOYMENT_ENVIRONMENT_NAME, "develop"),
            ],
            SCHEMA_URL,
        )
        .build()
}

fn otel() -> Result<Tracer, BoxError> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint("http://localhost:4317")
        .build()?;

    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_resource(resource())
        .with_batch_exporter(exporter)
        .build();

    opentelemetry::global::set_tracer_provider(provider.clone());
    let tracer = provider.tracer("tracing-otel-subscriber");

    Ok(tracer)
}

fn init_tracing() -> Result<(), BoxError> {
    let filter = Targets::new()
        .with_target("hyperdriver", Level::TRACE)
        .with_target(NAME, Level::TRACE)
        .with_default(Level::INFO);

    let tracer = otel()?;

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .pretty()
                .with_span_events(FmtSpan::CLOSE)
                .with_filter(filter.clone()),
        )
        .with(
            tracing_opentelemetry::layer()
                .with_tracer(tracer)
                .with_filter(filter),
        )
        .init();
    Ok(())
}
