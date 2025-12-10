//! This example closely follows the parent example from the `hyper` repository,
//! to demonstrate using a single-threaded runtime.
//!

use std::cell::Cell;
use std::future::Future;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::pin::{pin, Pin};
use std::rc::Rc;
use std::task::{ready, Context, Poll};
use std::thread;

use futures_util::FutureExt;
use http_body_util::BodyExt;
use hyper::body::{Body as HttpBody, Bytes, Frame, Incoming};
use hyper::Request;
use hyper::{Error, Response};
use hyperdriver::bridge::io::TokioIo;
use hyperdriver::bridge::service::TowerHyperService;
use hyperdriver::client::conn::protocol::auto;
use hyperdriver::client::conn::protocol::auto::HttpConnection;
use hyperdriver::client::conn::transport::tcp::{TcpConnectionError, TcpTransport};
use hyperdriver::client::conn::transport::TransportExt;
use hyperdriver::client::conn::Transport;
use hyperdriver::client::pool::{Pooled, UriKey};
use hyperdriver::client::ConnectionPoolLayer;
use hyperdriver::info::HasConnectionInfo;
use hyperdriver::server::Accept;
use hyperdriver::service::{make_service_fn, RequestExecutor};
use hyperdriver::stream::TcpStream;
use pin_project::pin_project;
use tokio::io::{self, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tower::service_fn;

struct Body {
    // Our Body type is !Send and !Sync:
    // _marker: PhantomData<*const ()>,
    _marker: PhantomData<()>,
    data: Option<Bytes>,
}

impl From<String> for Body {
    fn from(a: String) -> Self {
        Body {
            _marker: PhantomData,
            data: Some(a.into()),
        }
    }
}

impl HttpBody for Body {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(self.get_mut().data.take().map(|d| Ok(Frame::data(d))))
    }
}

fn main() {
    // pretty_env_logger::init();

    let (tx, rx) = oneshot::channel::<()>();
    let server_http2 = thread::spawn(move || {
        // Configure a runtime for the server that runs everything on the current thread
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime");

        // Combine it with a `LocalSet,  which means it can spawn !Send futures...
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, http2_server(rx)).unwrap();
    });

    let client_http2 = thread::spawn(move || {
        // Configure a runtime for the client that runs everything on the current thread
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime");

        // Combine it with a `LocalSet,  which means it can spawn !Send futures...
        let local = tokio::task::LocalSet::new();
        local
            .block_on(
                &rt,
                http2_client("http://localhost:3000".parse::<hyper::Uri>().unwrap(), tx),
            )
            .unwrap();
    });

    let (tx, rx) = oneshot::channel::<()>();

    let server_http1 = thread::spawn(move || {
        // Configure a runtime for the server that runs everything on the current thread
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime");

        // Combine it with a `LocalSet,  which means it can spawn !Send futures...
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, http1_server(rx)).unwrap();
    });

    let client_http1 = thread::spawn(move || {
        // Configure a runtime for the client that runs everything on the current thread
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime");

        // Combine it with a `LocalSet,  which means it can spawn !Send futures...
        let local = tokio::task::LocalSet::new();
        local
            .block_on(
                &rt,
                http1_client("http://localhost:3001".parse::<hyper::Uri>().unwrap(), tx),
            )
            .unwrap();
    });

    server_http2.join().unwrap();
    client_http2.join().unwrap();

    server_http1.join().unwrap();
    client_http1.join().unwrap();
}

async fn http1_server(rx: oneshot::Receiver<()>) -> Result<(), Box<dyn std::error::Error>> {
    let addr = SocketAddr::from(([127, 0, 0, 1], 3001));

    let listener = TcpListener::bind(addr).await?;

    // For each connection, clone the counter to use in our service...
    let counter = Rc::new(Cell::new(0));

    let mut rx = pin!(rx);

    loop {
        let (stream, addr) = tokio::select! {
            _ = &mut rx => return Ok(()),
            res = listener.accept() => res?,
        };

        let io = IOTypeNotSend::new(TcpStream::server(stream, addr));

        let cnt = counter.clone();

        let service = service_fn(move |_| {
            let prev = cnt.get();
            cnt.set(prev + 1);
            let value = cnt.get();
            async move {
                Ok::<_, Error>(Response::new(Body::from(format!(
                    "HTTP/1.1 Request #{value}"
                ))))
            }
        });

        tokio::task::spawn_local(async move {
            if let Err(err) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(io), TowerHyperService::new(service))
                .await
            {
                println!("Error serving connection: {err:?}");
            }
        });
    }
}

async fn http1_client(
    url: hyper::Uri,
    tx: oneshot::Sender<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    let host = url.host().expect("uri has no host");
    let port = url.port_u16().unwrap_or(80);
    let addr = format!("{host}:{port}");
    let stream = TcpStream::connect(addr).await?;

    let io = TokioIo::new(IOTypeNotSend::new(stream));

    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;

    tokio::task::spawn_local(async move {
        if let Err(err) = conn.await {
            let mut stdout = io::stdout();
            stdout
                .write_all(format!("Connection failed: {err:?}").as_bytes())
                .await
                .unwrap();
            stdout.flush().await.unwrap();
        }
    });

    let authority = url.authority().unwrap().clone();

    // Make 4 requests
    for _ in 0..4 {
        let req = Request::builder()
            .uri(url.clone())
            .header(hyper::header::HOST, authority.as_str())
            .body(Body::from("test".to_string()))?;

        let mut res = sender.send_request(req).await?;

        let mut stdout = io::stdout();
        stdout
            .write_all(format!("Response: {}\n", res.status()).as_bytes())
            .await
            .unwrap();
        stdout
            .write_all(format!("Headers: {:#?}\n", res.headers()).as_bytes())
            .await
            .unwrap();
        stdout.flush().await.unwrap();

        // Print the response body
        while let Some(next) = res.frame().await {
            let frame = next?;
            if let Some(chunk) = frame.data_ref() {
                stdout.write_all(chunk).await.unwrap();
            }
        }
        stdout.write_all(b"\n-----------------\n").await.unwrap();
        stdout.flush().await.unwrap();
    }

    let _ = tx.send(());

    Ok(())
}

async fn http2_server(rx: oneshot::Receiver<()>) -> Result<(), Box<dyn std::error::Error>> {
    use hyper::server::conn::http2;

    let mut stdout = io::stdout();

    let addr: SocketAddr = ([127, 0, 0, 1], 3000).into();
    // Using a !Send request counter is fine on 1 thread...
    let counter = Rc::new(Cell::new(0));

    let listener = TcpListener::bind(addr).await?;

    stdout
        .write_all(format!("Listening on http://{addr}").as_bytes())
        .await
        .unwrap();
    stdout.flush().await.unwrap();

    let server = hyperdriver::Server::builder()
        .with_acceptor(AcceptNotSend::new(listener))
        .with_protocol(http2::Builder::new(LocalExec))
        .with_make_service(make_service_fn(|_| {
            let counter = counter.clone();
            async move {
                Ok::<_, Error>(service_fn(move |_: http::Request<Incoming>| {
                    let prev = counter.get();
                    counter.set(prev + 1);
                    let value = counter.get();
                    async move {
                        Ok::<_, Error>(Response::new(Body::from(format!(
                            "HTTP/2 Request #{value}"
                        ))))
                    }
                }))
            }
        }))
        .with_executor(LocalExec);

    // static_assertions::assert_impl_one!(http2::Builder<LocalExec>: hyperdriver::server::Protocol<SharedService<http::Request<hyper::body::Incoming>, http::Response<Body>, io::Error>, IOTypeNotSend, hyper::body::Incoming>);
    // static_assertions::assert_impl_one!(LocalExec: ServerExecutor<http2::Builder<LocalExec>, BoxMakeServiceRef<IOTypeNotSend, SharedService<http::Request<Body>, http::Response<Body>, io::Error>, io::Error>, AcceptNotSend, Body>);

    server
        .with_graceful_shutdown(async {
            let _ = rx.await;
        })
        .await?;

    Ok(())
}

async fn http2_client(
    url: hyper::Uri,
    tx: oneshot::Sender<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = tower::ServiceBuilder::new()
        .layer(
            ConnectionPoolLayer::<_, _, _, UriKey>::new(
                TransportNotSend {
                    tcp: TcpTransport::<_, TcpStream>::default(),
                }
                .without_tls(),
                auto::HttpConnectionBuilder::<Body>::default(),
            )
            .with_optional_pool(Some(Default::default())),
        )
        .service(RequestExecutor::<Pooled<HttpConnection<Body>, Body>, _>::new());

    let authority = url.authority().unwrap().clone();

    // Make 4 requests
    for _ in 0..4 {
        let req = Request::builder()
            .uri(url.clone())
            .version(http::Version::HTTP_2)
            .header(http::header::HOST, authority.as_str())
            .body(Body::from("test".to_string()))?;

        let mut res = client.request(req).await?;

        let mut stdout = io::stdout();
        stdout
            .write_all(format!("Response: {}\n", res.status()).as_bytes())
            .await
            .unwrap();
        stdout
            .write_all(format!("Headers: {:#?}\n", res.headers()).as_bytes())
            .await
            .unwrap();
        stdout.flush().await.unwrap();

        // Print the response body
        while let Some(next) = res.frame().await {
            let frame = next?;
            if let Some(chunk) = frame.data_ref() {
                stdout.write_all(chunk).await.unwrap();
            }
        }
        stdout.write_all(b"\n-----------------\n").await.unwrap();
        stdout.flush().await.unwrap();
    }

    let _ = tx.send(());
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct LocalExec;

impl<F> hyper::rt::Executor<F> for LocalExec
where
    F: std::future::Future + 'static, // not requiring `Send`
    F::Output: 'static,
{
    fn execute(&self, fut: F) {
        // This will spawn into the currently running `LocalSet`.
        tokio::task::spawn_local(fut);
    }
}

#[derive(Debug)]
#[pin_project]
struct AcceptNotSend(#[pin] TcpListener);

impl AcceptNotSend {
    fn new(listener: TcpListener) -> Self {
        Self(listener)
    }
}

impl Accept for AcceptNotSend {
    type Conn = IOTypeNotSend;

    type Error = std::io::Error;

    fn poll_accept(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Conn, Self::Error>> {
        let stream = ready!(self.project().0.poll_accept(cx)).map(IOTypeNotSend::new);
        Poll::Ready(stream)
    }
}

#[derive(Clone)]
struct TransportNotSend {
    tcp: TcpTransport,
}

impl Transport for TransportNotSend {
    type IO = TcpStream;

    type Error = TcpConnectionError;

    type Future = Pin<Box<dyn Future<Output = Result<Self::IO, Self::Error>> + Send>>;

    fn connect(&mut self, req: http::request::Parts) -> <Self as Transport>::Future {
        self.tcp.connect(req).boxed()
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), <Self as Transport>::Error>> {
        self.tcp.poll_ready(cx)
    }
}

struct IOTypeNotSend {
    _marker: PhantomData<*const ()>,
    stream: TcpStream,
}

impl IOTypeNotSend {
    fn new(stream: TcpStream) -> Self {
        Self {
            _marker: PhantomData,
            stream,
        }
    }
}

impl HasConnectionInfo for IOTypeNotSend {
    type Addr = <TcpStream as HasConnectionInfo>::Addr;

    fn info(&self) -> hyperdriver::info::ConnectionInfo<Self::Addr> {
        self.stream.info()
    }
}

impl tokio::io::AsyncWrite for IOTypeNotSend {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

impl tokio::io::AsyncRead for IOTypeNotSend {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
