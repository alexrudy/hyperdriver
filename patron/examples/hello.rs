use std::sync::Arc;

use http::Uri;
use http_body_util::BodyExt as _;
use patron::{Client, Connect, ConnectionProtocol};
use tokio::io::AsyncWriteExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

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
        .arg(clap::Arg::new("requests").short('n').long("count"))
        .get_matches();

    let mut client = Client::builder();

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
        client.with_tls(config);

        if args.get_flag("protocol") {
            client.conn().set_protocol(ConnectionProtocol::Http1);
        } else {
            client.conn().set_protocol(ConnectionProtocol::Http2);
        }
    }

    let client = client.build();

    let n = *args.get_one::<usize>("requests").unwrap_or(&10);
    let done = Arc::new(tokio::sync::Barrier::new(n + 1));

    let uri: Uri = args
        .get_one::<String>("uri")
        .expect("uri argument must be present")
        .parse()?;
    for _ in 1usize..=n {
        tokio::spawn(send(client.clone(), uri.clone(), done.clone()));
    }

    done.wait().await;

    let done = Arc::new(tokio::sync::Barrier::new(n + 1));
    for _ in 1usize..=n {
        tokio::spawn(send(client.clone(), uri.clone(), done.clone()));
    }

    done.wait().await;

    Ok(())
}

async fn send<C>(mut client: Client<C>, uri: Uri, barrier: Arc<tokio::sync::Barrier>)
where
    C: Connect + Clone,
{
    let res = client.get(uri).await.unwrap();

    let (parts, mut body) = res.into_parts();
    let mut stdout = tokio::io::stdout();
    let mut total = 0usize;
    while let Some(Ok(frame)) = body.frame().await {
        if let Some(data) = frame.data_ref() {
            total += data.len();
            stdout.write_all(&data).await.unwrap();
        }
    }

    let res = http::Response::from_parts(parts, ());

    println!("Response: {} - {:?} {total}", res.status(), res.version());
    barrier.wait().await;
}
