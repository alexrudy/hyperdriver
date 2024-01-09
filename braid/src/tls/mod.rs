//! Support for braided streams which include Transport Layer security
//! and so involve a negotiation component.

pub mod client;
pub(crate) mod info;
pub mod server;

pub use info::TlsConnectionInfo;
