#![allow(missing_docs)]

use std::pin::pin;

use futures_util::StreamExt as _;
use hyper::upgrade::Upgraded;
use hyperdriver::{bridge::io::TokioIo, client::conn::transport::duplex::DuplexTransport};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

async fn server_for_client_upgrade(
) -> Result<DuplexTransport, Box<dyn std::error::Error + Send + Sync>> {
    let (tx, incoming) = hyperdriver::stream::duplex::pair();

    let acceptor: hyperdriver::server::conn::Acceptor =
        hyperdriver::server::conn::Acceptor::from(incoming);

    tokio::spawn(tokio::time::timeout(
        TIMEOUT,
        serve_one_h1_upgrade(acceptor),
    ));

    Ok(DuplexTransport::new(1024, tx))
}

async fn serve_one_h1_upgrade(
    acceptor: hyperdriver::server::conn::Acceptor,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut acceptor = pin!(acceptor);
    let stream = acceptor.next().await.ok_or("no connection")??;

    let service = hyper::service::service_fn(upgrade_svc);

    let conn =
        hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service);

    conn.with_upgrades().await?;

    Ok(())
}

async fn upgrade_svc(
    mut request: http::Request<hyper::body::Incoming>,
) -> Result<http::Response<hyperdriver::Body>, Box<dyn std::error::Error + Send + Sync>> {
    if !request.headers().contains_key(http::header::UPGRADE) {
        return Ok(http::Response::builder()
            .status(http::StatusCode::BAD_REQUEST)
            .body(hyperdriver::Body::empty())?);
    }

    tokio::spawn(tokio::time::timeout(TIMEOUT, async move {
        let upgraded = hyper::upgrade::on(&mut request)
            .await
            .expect("[server] upgrade erorr");
        server_upgraded_io(upgraded)
            .await
            .expect("[server] upgraded protocol error");
    }));

    Ok(http::Response::builder()
        .status(http::StatusCode::SWITCHING_PROTOCOLS)
        .body(hyperdriver::Body::empty())?)
}

async fn server_upgraded_io(
    upgraded: Upgraded,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut upgraded = TokioIo::new(upgraded);
    let mut vec = vec![0; 5];
    upgraded.read_exact(&mut vec).await?;
    let req = String::from_utf8_lossy(&vec);
    println!("[server] client sent {req:?}");
    if req != "hello" {
        println!("[server] got unexpected request");
    }
    upgraded.write_all(b"world").await?;
    println!("[server] sent response");
    Ok(())
}

async fn clinet_upgraded_io(mut response: http::Response<hyperdriver::Body>) {
    if response.status() != http::StatusCode::SWITCHING_PROTOCOLS {
        panic!("Server didn't upgrade: {}", response.status());
    }

    let upgraded = hyper::upgrade::on(&mut response)
        .await
        .expect("upgrade error");
    let mut upgraded = TokioIo::new(upgraded);
    upgraded.write_all(b"hello").await.unwrap();
    println!("[client] sent hello");
    let mut vec = vec![0; 5];
    upgraded.read_exact(&mut vec).await.unwrap();
    let res = String::from_utf8_lossy(&vec);
    println!("[client] got {res:?}");
    assert_eq!(res, "world");
}

#[tokio::test]
async fn client_auto() {
    let transport = server_for_client_upgrade().await.unwrap();

    let mut client = hyperdriver::Client::builder()
        .with_auto_http()
        .with_default_pool()
        .with_default_tls()
        .with_transport(transport)
        .build();

    let request = http::Request::get("http://example.org")
        .header(http::header::UPGRADE, "test-hyperdriver")
        .body(hyperdriver::Body::empty())
        .unwrap();

    let response = client.request(request).await.unwrap();
    clinet_upgraded_io(response).await;
}

#[tokio::test]
async fn client_http1() {
    let transport = server_for_client_upgrade().await.unwrap();

    let mut client = hyperdriver::Client::builder()
        .with_protocol(hyper::client::conn::http1::Builder::new())
        .with_default_pool()
        .with_default_tls()
        .with_transport(transport)
        .build();

    let request = http::Request::get("http://example.org")
        .header(http::header::UPGRADE, "test-hyperdriver")
        .body(hyperdriver::Body::empty())
        .unwrap();

    let response = client.request(request).await.unwrap();

    clinet_upgraded_io(response).await;
}
