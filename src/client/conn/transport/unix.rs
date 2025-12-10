//! Unix Domain Socket transport implementation for client connections.
//!
//! This module contains the [`UnixExtensionTransport`] type, which is a [`tower::Service`] that connects to
//! Unix domain sockets. Unlike TCP transports, Unix sockets use filesystem paths as addresses.
//!
//! The transport extracts the socket path from the URI authority or path component and establishes
//! a connection to the Unix domain socket at that location.

use std::future::Future;

pub use chateau::client::conn::transport::unix::{UnixConnectionError, UnixRequest};
use chateau::stream::unix::{UnixAddr, UnixStream};

use crate::BoxFuture;

/// Transport which looks up the target unix address in the request extensions.
#[derive(Debug, Clone)]
pub struct UnixExtensionTransport<T> {
    transport: T,
}

impl<T> From<T> for UnixExtensionTransport<T> {
    fn from(transport: T) -> Self {
        Self { transport }
    }
}

impl<T> UnixExtensionTransport<T> {
    /// Create a new UnixExtension transport from a unix transport.
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

impl<T, B, F> tower::Service<&http::Request<B>> for UnixExtensionTransport<T>
where
    T: for<'a> tower::Service<
            UnixRequest<&'a http::Request<B>>,
            Error = UnixConnectionError,
            Response = UnixStream,
            Future = F,
        > + Clone
        + Send
        + 'static,
    F: Future<Output = Result<UnixStream, UnixConnectionError>> + Send + 'static,
    B: Send + 'static,
{
    type Response = UnixStream;

    type Error = UnixConnectionError;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.transport.poll_ready(cx)
    }

    fn call(&mut self, req: &http::Request<B>) -> Self::Future {
        let transport = self.transport.clone();
        let mut transport = std::mem::replace(&mut self.transport, transport);
        let address = req
            .extensions()
            .get::<UnixAddr>()
            .cloned()
            .ok_or(UnixConnectionError::NoAddress);
        let connector = address.map(|addr| transport.call(UnixRequest::new(req, addr)));

        Box::pin(async move {
            let stream = connector?.await?;
            Ok(stream)
        })
    }
}
