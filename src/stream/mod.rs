//! Utilities for working across types of streams inside a single connector,
//! to allow the upstream routing table to select the most appropriate type of
//! conenction.

#![warn(missing_docs)]
#![deny(unsafe_code)]

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "stream")]
mod core;
pub mod duplex;
pub mod info;

#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "tls")]
pub mod tls;

#[cfg(feature = "stream")]
pub use core::Braid;

#[cfg(feature = "tls")]
pub use tls::TlsBraid;
