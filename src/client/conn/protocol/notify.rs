//! Make transport connection info available to the http::Response from the client.

use std::{fmt, future::Future, marker::PhantomData, task::ready};

use pin_project::pin_project;

use crate::client::conn::connection::notify::WatchedConnection;
use crate::info::HasConnectionInfo;

use super::{Protocol, ProtocolRequest};

/// Extension trait to provide a method for wrapping
/// a protocol to provide transport connection info to the response.
pub trait ProtocolNotifyExt<IO, B> {
    /// Wrap this protocol so that a notification watcher is inserted in the response.
    fn with_notify(self) -> ProtocolWithNotify<Self>
    where
        Self: Sized;
}

impl<P, IO, B> ProtocolNotifyExt<IO, B> for P
where
    P: Protocol<IO, B>,
    IO: HasConnectionInfo,
{
    fn with_notify(self) -> ProtocolWithNotify<Self>
    where
        Self: Sized,
    {
        ProtocolWithNotify::new(self)
    }
}

/// Preserves the input transport connection info on the output protocol connection.
#[derive(Debug, Clone)]
pub struct ProtocolWithNotify<P> {
    protocol: P,
}

impl<P> ProtocolWithNotify<P> {
    /// Wrap a protocol
    pub fn new(protocol: P) -> Self {
        Self { protocol }
    }

    /// Access a reference to the inner protocol
    pub fn protocol(&self) -> &P {
        &self.protocol
    }

    /// Access a mutable reference to the inner protocol
    pub fn protocol_mut(&mut self) -> &mut P {
        &mut self.protocol
    }

    /// Unwrap the protocol into the inner value.
    pub fn into_inner(self) -> P {
        self.protocol
    }
}

impl<P, IO, B> tower::Service<ProtocolRequest<IO, B>> for ProtocolWithNotify<P>
where
    P: Protocol<IO, B>,
    IO: HasConnectionInfo,
{
    type Response = WatchedConnection<P::Connection>;
    type Error = <P as Protocol<IO, B>>::Error;
    type Future =
        ProtocolWithNotifyFuture<<P as Protocol<IO, B>>::Future, P::Response, Self::Error>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Protocol::poll_ready(&mut self.protocol, cx)
    }

    fn call(&mut self, req: ProtocolRequest<IO, B>) -> Self::Future {
        let future = Protocol::connect(&mut self.protocol, req.transport, req.version);
        ProtocolWithNotifyFuture::new(future)
    }
}

/// A future which preserves the input transport's connection info
/// for use with the output protocol.
#[pin_project]
pub struct ProtocolWithNotifyFuture<F, C, E> {
    #[pin]
    future: F,
    _conn: PhantomData<fn() -> (C, E)>,
}

impl<F, C, E> ProtocolWithNotifyFuture<F, C, E> {
    fn new(future: F) -> Self {
        Self {
            future,
            _conn: PhantomData,
        }
    }
}

impl<F, C, E> fmt::Debug for ProtocolWithNotifyFuture<F, C, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProtocolWithNotifyFuture").finish()
    }
}

impl<F, C, E> Future for ProtocolWithNotifyFuture<F, C, E>
where
    F: Future<Output = Result<C, E>>,
{
    type Output = Result<WatchedConnection<C>, E>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut this = self.project();
        let conn = ready!(this.future.as_mut().poll(cx));
        std::task::Poll::Ready(conn.map(|c| WatchedConnection::new(c)))
    }
}
