#[tokio::test]
async fn braided_unix() {
    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = tempfile::tempdir().unwrap();

    let incoming = tokio::net::UnixListener::bind(dir.path().join("braid.sock")).unwrap();

    let server = hyperdriver::stream::server::Acceptor::from(incoming);
    tokio::spawn(async move {
        let mut incoming = server.fuse();
        while let Some(Ok(mut stream)) = incoming.next().await {
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).await.unwrap();
            stream.write_all(&buf[..n]).await.unwrap();
        }
    });

    let mut conn = hyperdriver::client::conn::Stream::from(
        tokio::net::UnixStream::connect(dir.path().join("braid.sock"))
            .await
            .unwrap(),
    );

    let mut buf = [0u8; 1024];
    conn.write_all(b"hello world").await.unwrap();
    let n = conn.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"hello world");
}
