//! Test server graceful shutdown

use std::future::Future;
use std::pin::{pin, Pin};
use std::task::Context;
use std::task::Poll;

use futures_util::task::noop_waker;
use hyperdriver::server::conn::Acceptor;
use hyperdriver::stream::duplex::{self, DuplexClient};
use hyperdriver::{Body, Server};

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

fn poll_once<F>(fut: Pin<&mut F>) -> Poll<F::Output>
where
    F: Future,
{
    let noop_waker = noop_waker();
    let mut cx = Context::from_waker(&noop_waker);

    fut.poll(&mut cx)
}

// Very simple echo server that responds with the request body.
async fn echo(req: http::Request<Body>) -> Result<http::Response<Body>, BoxError> {
    Ok(http::Response::new(req.into_body()))
}

async fn pair() -> (Acceptor, DuplexClient) {
    let (client, incoming) = duplex::pair();
    let acceptor = Acceptor::from(incoming);

    (acceptor, client)
}

#[tokio::test]
async fn before_request() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (acceptor, _) = pair().await;

    let server = Server::builder()
        .with_acceptor(acceptor)
        .with_shared_service(tower::service_fn(echo))
        .with_auto_http()
        .with_tokio();

    let serve = server.with_graceful_shutdown(async {
        if rx.await.is_err() {
            tracing::error!("shutdown with err?");
        }
    });

    tx.send(()).unwrap();

    let poll = poll_once(pin!(serve));
    assert!(poll.is_ready());
}
