//! Make transport connection info available to the http::Response from the client.

use std::{fmt, future::Future, marker::PhantomData, task::ready};

use pin_project::pin_project;

use crate::{
    client::conn::connection::info::WithInfo,
    info::{ConnectionInfo, HasConnectionInfo},
    DebugLiteral,
};

use super::{Protocol, ProtocolRequest};

/// Extension trait to provide a method for wrapping
/// a protocol to provide transport connection info to the response.
pub trait ProtocolInfoExt<IO, B> {
    /// Wrap this protocol so that transport connection info is inserted in the response.
    fn with_info(self) -> ProtocolWithInfo<Self>
    where
        Self: Sized;
}

impl<P, IO, B> ProtocolInfoExt<IO, B> for P
where
    P: Protocol<IO, B>,
    IO: HasConnectionInfo,
{
    fn with_info(self) -> ProtocolWithInfo<Self>
    where
        Self: Sized,
    {
        ProtocolWithInfo::new(self)
    }
}

/// Preserves the input transport connection info on the output protocol connection.
#[derive(Debug, Clone)]
pub struct ProtocolWithInfo<P> {
    protocol: P,
}

impl<P> ProtocolWithInfo<P> {
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

impl<P, IO, B> tower::Service<ProtocolRequest<IO, B>> for ProtocolWithInfo<P>
where
    P: Protocol<IO, B>,
    IO: HasConnectionInfo,
{
    type Response = WithInfo<P::Connection, IO::Addr>;
    type Error = <P as Protocol<IO, B>>::Error;
    type Future =
        ProtocolWithInfoFuture<<P as Protocol<IO, B>>::Future, P::Response, Self::Error, IO::Addr>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Protocol::poll_ready(&mut self.protocol, cx)
    }

    fn call(&mut self, req: ProtocolRequest<IO, B>) -> Self::Future {
        let info = req.transport.info();
        let future = Protocol::connect(&mut self.protocol, req.transport, req.version);
        ProtocolWithInfoFuture::new(future, info)
    }
}

/// A future which preserves the input transport's connection info
/// for use with the output protocol.
#[pin_project]
pub struct ProtocolWithInfoFuture<F, C, E, A> {
    #[pin]
    future: F,
    info: Option<ConnectionInfo<A>>,
    _conn: PhantomData<fn() -> (C, E)>,
}

impl<F, C, E, A> ProtocolWithInfoFuture<F, C, E, A> {
    fn new(future: F, info: ConnectionInfo<A>) -> Self {
        Self {
            future,
            info: Some(info),
            _conn: PhantomData,
        }
    }
}

impl<F, C, E, A> fmt::Debug for ProtocolWithInfoFuture<F, C, E, A>
where
    A: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.info {
            Some(info) => f
                .debug_struct("ProtocolWithInfoFuture")
                .field("info", &info)
                .finish(),
            None => f
                .debug_struct("ProtocolWithInfoFuture")
                .field("info", &DebugLiteral("already polled"))
                .finish(),
        }
    }
}

impl<F, C, E, A> Future for ProtocolWithInfoFuture<F, C, E, A>
where
    F: Future<Output = Result<C, E>>,
{
    type Output = Result<WithInfo<C, A>, E>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut this = self.project();
        let conn = ready!(this.future.as_mut().poll(cx));
        let info = this.info.take().expect("Future polled after info taken");
        std::task::Poll::Ready(conn.map(|c| WithInfo::new(c, info)))
    }
}
