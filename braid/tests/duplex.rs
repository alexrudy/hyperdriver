#[tokio::test]
async fn braided_duplex() {
    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (client, incoming) = braid::duplex::DuplexClient::new("test".parse().unwrap());

    let server = braid::server::Acceptor::from(incoming);
    tokio::spawn(async move {
        let mut incoming = server.fuse();
        while let Some(Ok(mut stream)) = incoming.next().await {
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).await.unwrap();
            stream.write_all(&buf[..n]).await.unwrap();
        }
    });

    let mut conn = braid::client::Stream::from(
        client
            .connect(1024, braid::info::Protocol::Http(http::Version::HTTP_11))
            .await
            .unwrap(),
    );

    let mut buf = [0u8; 1024];
    conn.write_all(b"hello world").await.unwrap();
    let n = conn.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"hello world");
}
