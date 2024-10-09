use std::fmt;
use std::future::{Future, IntoFuture as _};
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::future::{Fuse, FutureExt as _};
use hyper::rt::Executor;
use pin_project::pin_project;
use tracing::debug;
use tracing::instrument::Instrumented;
use tracing::Span;

use crate::private::Sealed;
use crate::server::{Accept, Connection, MakeServiceRef, Protocol};
use crate::server::{CloseFuture, CloseReciever, CloseSender};

/// An executor suitable for spawning connection futures
/// and driving them to completion.
pub trait ServerExecutor<P, S, A, B>:
    Executor<
        ConnectionDriver<
            Instrumented<P::Connection>,
            <P as Protocol<S::Service, A::Conn, B>>::Error,
        >,
    > + Sealed<(P, S, A, B)>
where
    P: Protocol<S::Service, A::Conn, B>,
    S: MakeServiceRef<A::Conn, B>,
    A: Accept,
{
}

impl<P, S, A, B, E> ServerExecutor<P, S, A, B> for E
where
    P: Protocol<S::Service, A::Conn, B>,
    S: MakeServiceRef<A::Conn, B>,
    A: Accept,
    E: Executor<
        ConnectionDriver<
            Instrumented<P::Connection>,
            <P as Protocol<S::Service, A::Conn, B>>::Error,
        >,
    >,
{
}

impl<P, S, A, B, E> Sealed<(P, S, A, B)> for E {}

/// Internal struct to manage driving a connection
/// to completion.
///
/// Only used in the bounds for ServerExecutor.
#[derive(Debug)]
#[pin_project]
pub struct ConnectionDriver<C, E> {
    #[pin]
    conn: C,
    _phantom: PhantomData<fn() -> E>,
}

impl<C, E> ConnectionDriver<C, E> {
    pub(in crate::server) fn new(conn: C) -> Self {
        ConnectionDriver {
            conn,
            _phantom: PhantomData,
        }
    }
}

impl<C, E> Future for ConnectionDriver<C, E>
where
    C: Future<Output = Result<(), E>>,
    E: fmt::Debug,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        match this.conn.poll(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(()),
            Poll::Ready(Err(e)) => {
                debug!("connection error: {:?}", e);
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// An executor suitable for spawning connection futures
/// and driving them to completion.
pub trait GracefulServerExecutor<P, S, A, B>:
    Executor<
        GracefulConnectionDriver<
            Instrumented<P::Connection>,
            <P as Protocol<S::Service, A::Conn, B>>::Error,
        >,
    > + ServerExecutor<P, S, A, B>
    + Sealed<(P, S, A, B, ())>
where
    P: Protocol<S::Service, A::Conn, B>,
    S: MakeServiceRef<A::Conn, B>,
    A: Accept,
{
}

impl<P, S, A, B, E> GracefulServerExecutor<P, S, A, B> for E
where
    P: Protocol<S::Service, A::Conn, B>,
    S: MakeServiceRef<A::Conn, B>,
    A: Accept,
    E: ServerExecutor<P, S, A, B>
        + Executor<
            GracefulConnectionDriver<
                Instrumented<P::Connection>,
                <P as Protocol<S::Service, A::Conn, B>>::Error,
            >,
        > + Sealed<(P, S, A, B, ())>,
{
}

impl<P, S, A, B, E> Sealed<(P, S, A, B, ())> for E
where
    P: Protocol<S::Service, A::Conn, B>,
    S: MakeServiceRef<A::Conn, B>,
    A: Accept,
    E: Executor<
        GracefulConnectionDriver<
            Instrumented<P::Connection>,
            <P as Protocol<S::Service, A::Conn, B>>::Error,
        >,
    >,
{
}

/// Internal struct to manage driving a connection to completion while
/// handling graceful shutdown.
#[pin_project]
pub struct GracefulConnectionDriver<C, E> {
    #[pin]
    conn: ConnectionDriver<C, E>,
    #[pin]
    shutdown: Fuse<CloseFuture>,
    finished: CloseSender,
    span: Span,
}

impl<C, E> fmt::Debug for GracefulConnectionDriver<C, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GracefulConnectionDriver").finish()
    }
}

impl<C, E> GracefulConnectionDriver<C, E> {
    pub(in crate::server) fn new(
        conn: C,
        shutdown: CloseReciever,
        finished: CloseSender,
        span: Span,
    ) -> Self {
        Self {
            conn: ConnectionDriver::new(conn),
            shutdown: shutdown.into_future().fuse(),
            finished,
            span,
        }
    }
}

impl<C, E> Future for GracefulConnectionDriver<Instrumented<C>, E>
where
    C: Connection + Future<Output = Result<(), E>>,
    E: fmt::Debug,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        let _guard = this.span.enter();

        loop {
            match this.conn.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    this.finished.send();
                    return Poll::Ready(());
                }
                Poll::Pending => {}
            };

            match this.shutdown.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    debug!("connection received shutdown signal");
                    this.conn
                        .as_mut()
                        .project()
                        .conn
                        .inner_pin_mut()
                        .graceful_shutdown();
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
