//! Hyperdriver
//!
//! Building the missing middle for network services in Rust.

#![cfg_attr(docsrs, feature(doc_auto_cfg))]

use std::fmt;

use tracing::dispatcher;

pub use body::Body;
#[cfg(feature = "client")]
pub use client::Client;
pub use hyperdriver_body as body;
#[cfg(feature = "client")]
pub use hyperdriver_client as client;
pub use hyperdriver_core::bridge;
#[cfg(feature = "discovery")]
pub mod discovery;

#[cfg(feature = "pidfile")]
pub mod pidfile;
pub use hyperdriver_core::info;
pub use hyperdriver_core::stream;
#[cfg(feature = "server")]
pub use hyperdriver_server as server;
#[cfg(feature = "server")]
pub use hyperdriver_server::Server;

/// `tower::Service` and `tower::Layer` implementations
/// relevant to hyperdriver.
pub mod service {
    pub use hyperdriver_body::service::*;
    pub use hyperdriver_core::service::*;
}

#[allow(unused)]
pub(crate) struct DebugLiteral<T: fmt::Display>(T);

impl<T: fmt::Display> fmt::Debug for DebugLiteral<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[allow(unused)]
#[track_caller]
pub(crate) fn polled_span(span: &tracing::Span) {
    dispatcher::get_default(|dispatch| {
        let id = span.id().expect("Missing ID; this is a bug");
        if let Some(current) = dispatch.current_span().id() {
            dispatch.record_follows_from(&id, current)
        }
    });
}

pub(crate) mod private {

    #[allow(unused)]
    pub trait Sealed {}
}
