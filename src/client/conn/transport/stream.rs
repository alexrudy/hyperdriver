use ::http::Uri;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use tower::Service;

use crate::client::stream::Stream;
use crate::info::BraidAddr;
use crate::info::HasConnectionInfo;

use super::Transport;
use super::TransportStream;

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
    type Response = TransportStream<Stream>;
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

/// Extension trait for Transports to provide a method to convert them into a transport
/// which uses a hyperdriver Braided stream.
pub trait TransportExt: Transport {
    /// Wrap the transport in a converter which produces a Stream
    fn into_stream(self) -> IntoStream<Self>
    where
        Self::IO: Into<Stream> + AsyncRead + AsyncWrite + Unpin + Send + 'static,
        <<Self as Transport>::IO as HasConnectionInfo>::Addr: Into<BraidAddr>,
    {
        IntoStream::new(self)
    }
}

impl<T> TransportExt for T where T: Transport {}

mod fut {

    use pin_project::pin_project;

    use crate::client::conn::{Transport, TransportStream};
    use crate::client::stream::Stream;
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
        type Output = Result<TransportStream<Stream>, T::Error>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            self.project()
                .future
                .poll(cx)
                .map_ok(|io| io.map(Into::into))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::client::conn::DuplexTransport;
    use crate::stream::server::AcceptExt as _;
    use tower::ServiceExt as _;

    #[tokio::test]
    async fn transport_into_stream() {
        let (client, srv) = crate::stream::duplex::pair("example.com".parse().unwrap());

        let transport = DuplexTransport::new(1024, None, client).into_stream();

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

        assert_eq!(info.protocol, None);
        assert_eq!(info.local_addr, BraidAddr::Duplex);
        assert_eq!(info.remote_addr, BraidAddr::Duplex);
    }
}
