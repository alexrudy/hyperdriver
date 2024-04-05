use core::fmt;

use tracing::dispatcher;

pub mod body;
pub mod bridge;
pub mod client;
pub mod discovery;
mod lazy;
pub mod pidfile;
mod rewind;
pub mod server;
pub mod stream;

pub(crate) struct DebugLiteral<T: fmt::Display>(T);

impl<T: fmt::Display> fmt::Debug for DebugLiteral<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub(crate) fn polled_span(span: &tracing::Span) {
    dispatcher::get_default(|dispatch| {
        let id = span.id().expect("Missing ID; this is a bug");
        if let Some(current) = dispatch.current_span().id() {
            dispatch.record_follows_from(&id, current)
        }
    });
}

pub(crate) mod private {
    pub trait Sealed {}
}
