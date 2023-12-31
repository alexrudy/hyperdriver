use http::Uri;
use http_body_util::BodyExt as _;
use patron::{Client, ConnectionProtocol};
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
        client.conn().set_protocol(ConnectionProtocol::Http2);
    }

    client.conn();

    let mut client = client.build();

    let uri: Uri = args
        .get_one::<String>("uri")
        .expect("uri argument must be present")
        .parse()?;
    let res = client.get(uri.clone()).await?;

    println!("Response: {} - {:?}", res.status(), res.version());

    for (name, value) in res.headers() {
        if let Ok(value) = value.to_str() {
            println!("  {}: {}", name, value);
        }
    }
    let mut body = res.into_body();
    let mut stdout = tokio::io::stdout();
    let mut total = 0usize;
    while let Some(Ok(frame)) = body.frame().await {
        if let Some(data) = frame.data_ref() {
            total += data.len();
            stdout.write_all(&data).await?;
        }
    }

    println!("Recieved {} body bytes", total);

    Ok(())
}
