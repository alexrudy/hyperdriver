use std::future::Future;
use std::future::IntoFuture;
use std::pin::pin;

use futures_util::FutureExt as _;
use http_body_util::BodyExt as _;
use hyper::body::Incoming;
use hyper::Response;
use hyperdriver::bridge::rt::TokioExecutor;
use hyperdriver::client::conn::Connection as _;
use hyperdriver::stream::client::Stream;
use hyperdriver::stream::info::HasConnectionInfo;
use hyperdriver::stream::server::Accept;
use tower::MakeService;

use hyperdriver::server::{Protocol, Server};

async fn echo(
    req: hyper::Request<Incoming>,
) -> Result<hyper::Response<hyperdriver::body::Body>, hyper::Error> {
    tracing::trace!("processing request");
    let body = req.into_body();
    let data = body.collect().await?;
    tracing::trace!("collected body, responding");
    Ok(Response::new(hyperdriver::body::Body::from(
        data.to_bytes(),
    )))
}

async fn connection<P: hyperdriver::client::Protocol<Stream>>(
    client: &hyperdriver::stream::duplex::DuplexClient,
    mut protocol: P,
) -> Result<P::Connection, Box<dyn std::error::Error>> {
    let stream = client.connect(1024, None).await?;
    let conn = protocol
        .connect(hyperdriver::client::TransportStream::new(stream.into()).await?)
        .await?;
    Ok(conn)
}

fn hello_world() -> hyperdriver::body::Request {
    http::Request::builder()
        .uri("/")
        .body(hyperdriver::body::Body::from("hello world"))
        .unwrap()
}

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

fn serve<S, P, A>(server: Server<S, P, A>) -> impl Future<Output = Result<(), BoxError>>
where
    S: MakeService<
            (),
            hyper::Request<hyper::body::Incoming>,
            Response = hyperdriver::body::Response,
        > + Send
        + 'static,
    S::Service: Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    S::MakeError: std::error::Error + Send + Sync + 'static,
    <S::Service as tower::Service<hyper::Request<hyper::body::Incoming>>>::Future: Send + 'static,
    P: Protocol<S::Service, A::Conn> + Send + 'static,
    A: Accept + Send + Unpin + 'static,
    A::Conn: HasConnectionInfo + Send + 'static,
    A::Error: std::error::Error + Send + Sync + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        let rx = pin!(rx.fuse());
        let serve = pin!(server.into_future());

        tokio::select! {
            rv = serve => {
                rv.map_err(BoxError::from)
            },
            _ = rx => {
                Ok(())
            },
        }
    });
    tracing::trace!("spawned server");

    async move {
        tracing::trace!("sending shutdown signal");
        let _ = tx.send(());
        handle.await.unwrap().map_err(Into::into)
    }
}

fn serve_gracefully<S, P, A>(server: Server<S, P, A>) -> impl Future<Output = Result<(), BoxError>>
where
    S: MakeService<
            (),
            hyper::Request<hyper::body::Incoming>,
            Response = hyperdriver::body::Response,
        > + Send
        + 'static,
    S::Service: Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    S::MakeError: std::error::Error + Send + Sync + 'static,
    <S::Service as tower::Service<hyper::Request<hyper::body::Incoming>>>::Future: Send + 'static,
    P: Protocol<S::Service, A::Conn> + Send + 'static,
    A: Accept + Send + Unpin + 'static,
    A::Conn: HasConnectionInfo + Send + 'static,
    A::Error: std::error::Error + Send + Sync + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(server.with_graceful_shutdown(async {
        if rx.await.is_err() {
            tracing::trace!("shutdown with err?");
        }
    }));
    tracing::trace!("spawned server");

    async move {
        tracing::trace!("sending shutdown signal");
        let _ = tx.send(());
        handle.await.unwrap().map_err(Into::into)
    }
}

#[tokio::test]
async fn echo_h1() {
    use hyper::client::conn::http1::Builder;
    let _ = tracing_subscriber::fmt::try_init();

    let (client, incoming) = hyperdriver::stream::duplex::pair("test".parse().unwrap());

    let acceptor = hyperdriver::stream::server::Acceptor::from(incoming);
    let server = hyperdriver::server::Server::new_with_protocol(
        acceptor,
        tower::service_fn(|_| async { Ok::<_, hyper::Error>(tower::service_fn(echo)) }),
        hyperdriver::server::conn::http1::Builder::new(),
    );

    let handle = serve_gracefully(server);

    let mut conn = connection(&client, Builder::new()).await.unwrap();
    tracing::trace!("connected");

    let response = conn.send_request(hello_world()).await.unwrap();
    tracing::trace!("sent request");
    let (_, body) = response.into_parts();

    let data = body.collect().await.unwrap().to_bytes();
    assert_eq!(&*data, b"hello world");

    handle.await.unwrap();
}

#[tokio::test]
async fn echo_h2() {
    use hyper::client::conn::http2::Builder;

    let _ = tracing_subscriber::fmt::try_init();

    let (client, incoming) = hyperdriver::stream::duplex::pair("test".parse().unwrap());

    let acceptor = hyperdriver::stream::server::Acceptor::from(incoming);
    let server = hyperdriver::server::Server::new_with_protocol(
        acceptor,
        tower::service_fn(|_| async { Ok::<_, hyper::Error>(tower::service_fn(echo)) }),
        hyperdriver::server::conn::http2::Builder::new(TokioExecutor::new()),
    );

    let guard = serve_gracefully(server);

    let mut conn = connection(&client, Builder::new(TokioExecutor::new()))
        .await
        .unwrap();

    let response = conn.send_request(hello_world()).await.unwrap();
    let (_, body) = response.into_parts();

    let data = body.collect().await.unwrap().to_bytes();
    assert_eq!(&*data, b"hello world");

    guard.await.unwrap();
}

#[tokio::test]
async fn echo_h1_early_disconnect() {
    use hyper::client::conn::http1::Builder;
    let _ = tracing_subscriber::fmt::try_init();

    let (client, incoming) = hyperdriver::stream::duplex::pair("test".parse().unwrap());

    let acceptor = hyperdriver::stream::server::Acceptor::from(incoming);
    let server = hyperdriver::server::Server::new_with_protocol(
        acceptor,
        tower::service_fn(|_| async { Ok::<_, hyper::Error>(tower::service_fn(echo)) }),
        hyperdriver::server::conn::http1::Builder::new(),
    );

    let handle = serve(server);

    let mut conn = connection(&client, Builder::new()).await.unwrap();
    drop(client);
    tracing::trace!("connected");

    let response = conn.send_request(hello_world()).await.unwrap();
    tokio::task::yield_now().await;

    tracing::trace!("sent request");
    let (_, body) = response.into_parts();
    let data = body.collect().await.unwrap().to_bytes();
    assert_eq!(&*data, b"hello world");

    assert!(handle.await.is_err());
}

#[tokio::test]
async fn echo_auto_h1() {
    let _ = tracing_subscriber::fmt::try_init();

    let (client, incoming) = hyperdriver::stream::duplex::pair("test".parse().unwrap());

    let acceptor = hyperdriver::stream::server::Acceptor::from(incoming);
    let server = hyperdriver::server::Server::new_with_protocol(
        acceptor,
        tower::service_fn(|_| async { Ok::<_, hyper::Error>(tower::service_fn(echo)) }),
        hyperdriver::server::conn::auto::Builder::new(TokioExecutor::new()),
    );

    let handle = serve_gracefully(server);

    let mut conn = connection(&client, hyper::client::conn::http1::Builder::new())
        .await
        .unwrap();
    tracing::trace!("connected");

    let response = conn.send_request(hello_world()).await.unwrap();
    tracing::trace!("sent request");
    let (_, body) = response.into_parts();

    let data = body.collect().await.unwrap().to_bytes();
    assert_eq!(&*data, b"hello world");

    handle.await.unwrap();
}

#[tokio::test]
async fn echo_auto_h2() {
    let _ = tracing_subscriber::fmt::try_init();

    let (client, incoming) = hyperdriver::stream::duplex::pair("test".parse().unwrap());

    let acceptor = hyperdriver::stream::server::Acceptor::new(incoming);
    let server = hyperdriver::server::Server::new_with_protocol(
        acceptor,
        tower::service_fn(|_| async { Ok::<_, hyper::Error>(tower::service_fn(echo)) }),
        hyperdriver::server::conn::auto::Builder::new(TokioExecutor::new()),
    );

    let handle = serve_gracefully(server);

    let mut conn = connection(
        &client,
        hyper::client::conn::http2::Builder::new(TokioExecutor::new()),
    )
    .await
    .unwrap();
    tracing::trace!("connected");

    let response = conn.send_request(hello_world()).await.unwrap();
    tracing::trace!("sent request");
    let (_, body) = response.into_parts();

    let data = body.collect().await.unwrap().to_bytes();
    assert_eq!(&*data, b"hello world");

    handle.await.unwrap();
}
