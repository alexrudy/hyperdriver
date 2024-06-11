//! Hyperdriver
//!
//! Building the missing middle for network services in Rust.

#![deny(missing_docs)]
#![deny(missing_debug_implementations)]

use core::fmt;

use tracing::dispatcher;

pub mod body;
pub use body::Body;
pub mod bridge;

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "client")]
pub use client::Client;

#[cfg(feature = "discovery")]
pub mod discovery;

#[cfg(feature = "client")]
pub(crate) mod happy_eyeballs;

#[cfg(feature = "client")]
mod lazy;

#[cfg(feature = "pidfile")]
pub mod pidfile;

#[cfg(feature = "server")]
mod rewind;

#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "server")]
pub use server::Server;

pub mod stream;

#[allow(unused)]
pub(crate) struct DebugLiteral<T: fmt::Display>(T);

impl<T: fmt::Display> fmt::Debug for DebugLiteral<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[allow(unused)]
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

/// Service-related types and traits.
pub mod service {

    pub use crate::body::{AdaptCustomBodyExt, AdaptCustomBodyLayer, AdaptCustomBodyService};
    pub use crate::body::{AdaptIncomingLayer, AdaptIncomingService};
    pub use crate::body::{AdaptOuterBodyLayer, AdaptOuterBodyService};

    #[cfg(feature = "server")]
    pub use crate::server::service::{make_service_fn, HttpService, MakeServiceRef};
    pub use tower::{service_fn, Service, ServiceBuilder, ServiceExt};
}
