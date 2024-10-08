use ::http::Uri;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use tower::Service;

use crate::client::conn::Stream;
use crate::info::BraidAddr;
use crate::info::HasConnectionInfo;

use super::Transport;

/// A transport which can be converted into a stream.
#[derive(Debug, Clone)]
pub struct IntoStream<T> {
    transport: T,
}

impl<T> IntoStream<T> {
    /// Create a new `IntoStream` transport.
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

impl<T> Service<Uri> for IntoStream<T>
where
    T: Transport,
    T::IO: Into<Stream> + AsyncRead + AsyncWrite + Unpin + Send + 'static,
    <<T as Transport>::IO as HasConnectionInfo>::Addr: Into<BraidAddr>,
{
    type Response = Stream;
    type Error = T::Error;
    type Future = fut::ConnectFuture<T>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.transport.poll_ready(cx)
    }

    fn call(&mut self, req: Uri) -> Self::Future {
        fut::ConnectFuture::new(self.transport.connect(req))
    }
}

mod fut {

    use pin_project::pin_project;

    use crate::client::conn::Stream;
    use crate::client::conn::Transport;
    use crate::info::{BraidAddr, HasConnectionInfo};

    /// Future returned by `IntoStream` transports.
    #[pin_project]
    #[derive(Debug)]
    pub struct ConnectFuture<T>
    where
        T: Transport,
    {
        #[pin]
        future: T::Future,
    }

    impl<T> ConnectFuture<T>
    where
        T: Transport,
    {
        pub(super) fn new(future: T::Future) -> Self {
            Self { future }
        }
    }

    impl<T> std::future::Future for ConnectFuture<T>
    where
        T: Transport,
        T::IO: Into<Stream>,
        <<T as Transport>::IO as HasConnectionInfo>::Addr: Into<BraidAddr>,
    {
        type Output = Result<Stream, T::Error>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            self.project().future.poll(cx).map_ok(|io| io.into())
        }
    }
}

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::*;

    use crate::client::conn::transport::duplex::DuplexTransport;
    use crate::client::conn::transport::TransportExt as _;
    use crate::server::conn::AcceptExt as _;
    use tower::ServiceExt as _;

    #[tokio::test]
    async fn transport_into_stream() {
        let (client, srv) = crate::stream::duplex::pair();

        let transport = DuplexTransport::new(1024, client).into_stream();

        let (io, _) = tokio::join!(
            async {
                transport
                    .oneshot("https://example.com".parse().unwrap())
                    .await
                    .unwrap()
            },
            async { srv.accept().await.unwrap() }
        );
        let info = io.info();

        assert_eq!(info.local_addr, BraidAddr::Duplex);
        assert_eq!(info.remote_addr, BraidAddr::Duplex);
    }
}
