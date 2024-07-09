//! Utilities for working across types of streams inside a single connector,
//! to allow the upstream routing table to select the most appropriate type of
//! conenction.

#[cfg(feature = "stream")]
mod core;
pub mod duplex;
pub mod tcp;
#[cfg(feature = "tls")]
pub mod tls;
pub mod unix;

#[cfg(feature = "stream")]
pub use core::Braid;
pub use tcp::TcpStream;
#[cfg(feature = "tls")]
pub use tls::TlsBraid;
pub use unix::UnixStream;
