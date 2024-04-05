//! Utilities for working across types of streams inside a single connector,
//! to allow the upstream routing table to select the most appropriate type of
//! conenction.

#![warn(missing_docs)]
#![deny(unsafe_code)]

pub mod client;
mod core;
pub mod duplex;
pub mod info;
pub mod server;
pub mod tls;

pub use core::{Braid, TlsBraid};
