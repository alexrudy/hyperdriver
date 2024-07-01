//! A simple HTTP client that uses the hyperdriver crate.
//!
//! This client can send HTTP requests to a server and print the response.
//! It provides a simple command-line interface, use `--help` to see the options.

use http::Uri;
use http_body_util::BodyExt as _;
use hyperdriver::client::Client;
use tokio::io::AsyncWriteExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let _ = rustls::crypto::ring::default_provider().install_default();

    let args = clap::Command::new("patron")
        .version(env!("CARGO_PKG_VERSION"))
        .about("HTTP/2 client")
        .arg(
            clap::Arg::new("uri")
                .help("The URI to request")
                .required(true),
        )
        .arg(
            clap::Arg::new("tls-root")
                .short('t')
                .long("tls")
                .alias("tls-root")
                .help("Where to find the TLS root"),
        )
        .arg(
            clap::Arg::new("protocol")
                .short('1')
                .long("http1")
                .action(clap::ArgAction::SetTrue)
                .help("Use HTTP/1.1"),
        )
        .arg(
            clap::Arg::new("requests")
                .short('n')
                .long("count")
                .value_parser(clap::value_parser!(usize)),
        )
        .get_matches();

    let mut client = Client::builder()
        .with_tcp(Default::default())
        .with_auto_http();

    if let Some(tls_root) = args.get_one::<String>("tls-root") {
        println!("Using TLS root: {}", tls_root);

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

    let version = if args.get_flag("protocol") {
        http::Version::HTTP_11
    } else {
        http::Version::HTTP_2
    };

    let client = client.build();

    let n = *args.get_one::<usize>("requests").unwrap_or(&10);
    let (dtx, mut drx) = tokio::sync::mpsc::channel(n);

    let uri: Uri = args
        .get_one::<String>("uri")
        .expect("uri argument must be present")
        .parse()?;
    for _ in 1usize..=n {
        tokio::spawn(send(client.clone(), uri.clone(), version, dtx.clone()));
    }

    drop(dtx);
    let _ = drx.recv().await;

    let (dtx, mut drx) = tokio::sync::mpsc::channel(n);
    for _ in 1usize..=n {
        tokio::spawn(send(client.clone(), uri.clone(), version, dtx.clone()));
    }

    drop(dtx);
    let _ = drx.recv().await;

    Ok(())
}

async fn send(
    mut client: Client,
    uri: Uri,
    version: http::Version,
    done: tokio::sync::mpsc::Sender<()>,
) {
    let req = http::Request::builder()
        .uri(uri.clone())
        .method("GET")
        .version(version)
        .body(hyperdriver::body::Body::empty())
        .unwrap();

    let res = match client.request(req).await {
        Ok(res) => res,
        Err(err) => {
            println!("Error: {}", err);
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
    drop(done);
}
