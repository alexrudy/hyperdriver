use std::{
    future::{Future, IntoFuture},
    pin::Pin,
    sync::Arc,
    task::Context,
};

use crate::BoxFuture;

#[derive(Debug, Clone)]
pub(crate) struct Sender(Option<tokio::sync::watch::Receiver<()>>);

impl Sender {
    pub(crate) fn send(&mut self) {
        let _ = self.0.take();
        tracing::trace!("sending close signal");
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Receiver(Arc<tokio::sync::watch::Sender<()>>);

impl IntoFuture for Receiver {
    type IntoFuture = Notified;
    type Output = ();

    fn into_future(self) -> Self::IntoFuture {
        Notified(Box::pin(async move {
            self.0.closed().await;
        }))
    }
}

#[pin_project::pin_project]
pub(crate) struct Notified(#[pin] BoxFuture<'static, ()>);

impl Future for Notified {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> std::task::Poll<Self::Output> {
        self.project().0.poll(cx)
    }
}

pub(crate) fn channel() -> (Sender, Receiver) {
    let (tx, rx) = tokio::sync::watch::channel(());
    (Sender(Some(rx)), Receiver(Arc::new(tx)))
}
