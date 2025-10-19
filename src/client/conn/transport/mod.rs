//! Transport streams for connecting to remote servers.
//!
//! Transports are responsible for establishing a connection to a remote server, shuffling bytes back and forth,

#[cfg(target_family = "unix")]
pub mod unix;
