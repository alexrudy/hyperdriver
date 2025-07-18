//! Test a hyperdriver::Server with a hyper::client in various configurations.
use std::future::Future;
use std::future::IntoFuture;
use std::pin::pin;

use futures_util::FutureExt as _;
use http_body::Body as HttpBody;
use http_body_util::BodyExt as _;
use hyper::Response;
use hyperdriver::bridge::rt::TokioExecutor;
use hyperdriver::client::conn::protocol::HttpProtocol;
use hyperdriver::client::conn::Stream;
use hyperdriver::info::BraidAddr;
use hyperdriver::server::conn::Accept;
use hyperdriver::server::GracefulServerExecutor;
use hyperdriver::server::ServerExecutor;
use hyperdriver::service::MakeServiceRef;
use hyperdriver::Body;

use hyperdriver::server::{Protocol, Server};
type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

async fn echo(req: http::Request<Body>) -> Result<http::Response<Body>, BoxError> {
    tracing::trace!("processing request");
    let info = req
        .extensions()
        .get::<hyperdriver::info::ConnectionInfo<BraidAddr>>();
    assert!(info.is_some(), "expected to find connection info");
    tracing::trace!("found connection info: {info:?}");

    let body = req.into_body();
    let data = body.collect().await?;
    tracing::trace!("collected body, responding");
    Ok(Response::new(hyperdriver::body::Body::from(
        data.to_bytes(),
    )))
}

async fn connection<P: hyperdriver::client::conn::Protocol<Stream, hyperdriver::Body>>(
    client: &hyperdriver::stream::duplex::DuplexClient,
    mut protocol: P,
) -> Result<P::Connection, Box<dyn std::error::Error>> {
    let stream = client.connect(1024).await?;
    let conn = protocol.connect(stream.into(), HttpProtocol::Http1).await?;
    Ok(conn)
}

fn hello_world() -> http::Request<Body> {
    http::Request::builder()
        .uri("https://localhost/hello")
        .body(Body::from("hello world"))
        .unwrap()
}

fn serve<A, P, S, BIn, E>(
    server: Server<A, P, S, BIn, E>,
) -> impl Future<Output = Result<(), BoxError>>
where
    S: MakeServiceRef<A::Conn, BIn> + Send + 'static,
    S::Future: Send + 'static,
    P: Protocol<S::Service, A::Conn, BIn> + Send + 'static,
    A: Accept + Unpin + Send + 'static,
    A::Conn: Send + 'static,
    BIn: HttpBody + Send + 'static,
    E: ServerExecutor<P, S, A, BIn> + Send + 'static,
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
        handle.await.unwrap()
    }
}

fn serve_gracefully<A, P, S, B, E>(
    server: Server<A, P, S, B, E>,
) -> impl Future<Output = Result<(), BoxError>>
where
    S: MakeServiceRef<A::Conn, B> + Send + 'static,
    S::Future: Send + 'static,
    P: Protocol<S::Service, A::Conn, B> + Send + 'static,
    A: Accept + Unpin + Send + 'static,
    A::Conn: Send + 'static,
    B: HttpBody + Send + 'static,
    E: GracefulServerExecutor<P, S, A, B> + Send + 'static,
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

    let (client, incoming) = hyperdriver::stream::duplex::pair();

    let server = hyperdriver::server::Server::builder()
        .with_incoming(incoming)
        .with_http1()
        .with_shared_service(tower::service_fn(echo))
        .with_connection_info()
        .with_tokio();

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

    let (client, incoming) = hyperdriver::stream::duplex::pair();

    let server = hyperdriver::server::Server::builder()
        .with_incoming(incoming)
        .with_http2()
        .with_shared_service(tower::service_fn(echo))
        .with_connection_info()
        .with_tokio();

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

    let (client, incoming) = hyperdriver::stream::duplex::pair();

    let server = hyperdriver::server::Server::builder()
        .with_incoming(incoming)
        .with_http1()
        .with_shared_service(tower::service_fn(echo))
        .with_connection_info()
        .with_tokio();

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

    let (client, incoming) = hyperdriver::stream::duplex::pair();

    let server = hyperdriver::Server::builder()
        .with_incoming(incoming)
        .with_auto_http()
        .with_shared_service(tower::service_fn(echo))
        .with_connection_info()
        .with_tokio();

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

    let (client, incoming) = hyperdriver::stream::duplex::pair();

    let server = hyperdriver::Server::builder()
        .with_incoming(incoming)
        .with_auto_http()
        .with_shared_service(tower::service_fn(echo))
        .with_connection_info()
        .with_tokio();
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
