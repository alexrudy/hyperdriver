use std::net::Ipv4Addr;

#[tokio::test]
async fn braided_tcp() {
    use futures::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let incoming =
        hyper::server::conn::AddrIncoming::bind(&(Ipv4Addr::LOCALHOST, 0).into()).unwrap();
    let addr = incoming.local_addr();

    let server = braid::server::acceptor::Acceptor::from(incoming);
    tokio::spawn(async move {
        let mut incoming = server.fuse();
        while let Some(Ok(mut stream)) = incoming.next().await {
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).await.unwrap();
            stream.write_all(&buf[..n]).await.unwrap();
        }
    });

    let mut conn = braid::client::Stream::from(tokio::net::TcpStream::connect(addr).await.unwrap());

    let mut buf = [0u8; 1024];
    conn.write_all(b"hello world").await.unwrap();
    let n = conn.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"hello world");
}
