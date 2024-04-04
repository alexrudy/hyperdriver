use core::fmt;

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
