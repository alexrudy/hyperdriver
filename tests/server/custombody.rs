use bytes::Bytes;
use futures_util::FutureExt;
use http_body::Body;
use http_body_util::combinators::UnsyncBoxBody;
use hyperdriver::{body::AdaptCustomBodyExt, info::DuplexAddr};
use pin_project::pin_project;

#[pin_project]
struct CustomBody(Option<Vec<u8>>);

impl Body for CustomBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        if let Some(data) = self.project().0.take() {
            std::task::Poll::Ready(Some(Ok(http_body::Frame::data(data.into()))))
        } else {
            std::task::Poll::Ready(None)
        }
    }
}

impl From<hyperdriver::Body> for CustomBody {
    fn from(_body: hyperdriver::Body) -> Self {
        let data = Vec::new();
        CustomBody(Some(data))
    }
}

impl From<UnsyncBoxBody<Bytes, Box<dyn std::error::Error + Send + Sync + 'static>>> for CustomBody {
    fn from(
        _body: UnsyncBoxBody<Bytes, Box<dyn std::error::Error + Send + Sync + 'static>>,
    ) -> Self {
        let data = Vec::new();
        CustomBody(Some(data))
    }
}

#[derive(Clone)]
struct CustomService;

impl tower::Service<http::Request<CustomBody>> for CustomService {
    type Response = http::Response<CustomBody>;
    type Error = std::convert::Infallible;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<CustomBody>) -> Self::Future {
        let info = req
            .extensions()
            .get::<hyperdriver::info::ConnectionInfo<DuplexAddr>>()
            .unwrap();
        assert_eq!(*info.remote_addr(), DuplexAddr::new());
        let mut incoming_body = req.into_body();
        let body = CustomBody(incoming_body.0.take());
        std::future::ready(Ok(http::Response::new(body)))
    }
}

#[tokio::test]
async fn custom_body_server() {
    let (_, incoming) = hyperdriver::stream::duplex::pair("test".parse().unwrap());
    let service = CustomService.adapt_custom_body();

    let server = hyperdriver::server::Server::builder()
        .with_acceptor(incoming)
        .with_auto_http()
        .with_shared_service(service)
        .with_connection_info();

    let (tx, rx) = tokio::sync::oneshot::channel();

    let fut = server.with_graceful_shutdown(async move {
        let _ = rx.await;
    });

    let _ = tx.send(());
    let _ = fut.now_or_never().unwrap();
}
