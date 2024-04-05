//! Utilities for working across types of streams inside a single connector,
//! to allow the upstream routing table to select the most appropriate type of
//! conenction.

#![warn(missing_docs)]
#![deny(unsafe_code)]

#[cfg(all(feature = "client", feature = "stream"))]
pub mod client;
mod core;
pub mod duplex;
pub mod info;

#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "tls")]
pub mod tls;

pub use core::Braid;

#[cfg(feature = "tls")]
pub use core::TlsBraid;
