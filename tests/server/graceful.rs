use std::future::Future;
use std::pin::{pin, Pin};
use std::task::Context;
use std::task::Poll;

use futures_util::task::noop_waker;
use hyperdriver::body::Request;
use hyperdriver::body::Response;
use hyperdriver::service::make_service_fn;
use hyperdriver::stream::duplex::{self, DuplexClient};
use hyperdriver::stream::server;
use hyperdriver::stream::server::Acceptor;
use hyperdriver::Server;

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
async fn echo(req: Request) -> Result<Response, BoxError> {
    Ok(Response::new(req.into_body()))
}

async fn pair() -> (Acceptor, DuplexClient) {
    let (client, incoming) = duplex::pair("test".parse().unwrap());
    let acceptor = server::Acceptor::from(incoming);

    (acceptor, client)
}

#[tokio::test]
async fn before_request() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (acceptor, _) = pair().await;

    let server = Server::new(
        acceptor,
        make_service_fn(|_| async { Ok::<_, BoxError>(tower::service_fn(echo)) }),
    );

    let serve = server.with_graceful_shutdown(async {
        if rx.await.is_err() {
            tracing::error!("shutdown with err?");
        }
    });

    tx.send(()).unwrap();

    let poll = poll_once(pin!(serve));
    assert!(poll.is_ready());
}
