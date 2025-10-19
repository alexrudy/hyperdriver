//! Utilities for working across types of streams inside a single connector,
//! to allow the upstream routing table to select the most appropriate type of
//! conenction.

#[cfg(feature = "stream")]
mod core;

#[cfg(feature = "stream")]
pub use core::Braid;

pub use chateau::stream::{duplex, tcp, unix};
