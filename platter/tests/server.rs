use std::{future::IntoFuture as _, pin::pin};

use bridge::io::TokioIo;
use hyper::body::Incoming;

#[tokio::test]
async fn platter_duplex() {
    use http_body_util::BodyExt;

    let (client, incoming) = braid::duplex::pair("test".parse().unwrap());

    let acceptor = braid::server::Acceptor::from(incoming);
    let server = platter::Server::new(
        acceptor,
        tower::service_fn(|_| async {
            Ok::<_, hyper::Error>(tower::service_fn(|req: http::Request<Incoming>| async {
                let body = req.into_body();
                let data = body.collect().await?;
                Ok::<_, hyper::Error>(hyper::Response::new(arnold::Body::from(data.to_bytes())))
            }))
        }),
    );

    let (tx, rx) = tokio::sync::oneshot::channel();

    let handle = tokio::spawn(async move {
        let mut server = pin!(server.into_future());

        tokio::select! {
            rv = &mut server => {
                rv
            },
            _ = rx => {
                Ok(())
            },
        }
    });

    let stream = braid::client::Stream::from(
        client
            .connect(
                1024,
                Some(braid::info::Protocol::Http(http::Version::HTTP_11)),
            )
            .await
            .unwrap(),
    );

    let (mut sender, conn) = hyper::client::conn::http1::Builder::new()
        .handshake::<_, arnold::Body>(TokioIo::new(stream))
        .await
        .unwrap();

    tokio::spawn(conn);

    let req = http::Request::builder()
        .uri("/")
        .body(arnold::Body::from("hello world"))
        .unwrap();

    let response = sender.send_request(req).await.unwrap();
    let (parts, body) = response.into_parts();
    assert_eq!(parts.status, http::StatusCode::OK);
    let data = body.collect().await.unwrap();
    assert_eq!(data.to_bytes(), b"hello world".as_slice());

    tx.send(()).unwrap();
    handle.await.unwrap().unwrap();
}
