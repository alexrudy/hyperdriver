mod adapt;

#[cfg(feature = "incoming")]
mod incoming;

#[cfg(feature = "incoming")]
pub use self::incoming::{AdaptIncomingLayer, AdaptIncomingService};

pub use self::adapt::AdaptCustomBodyExt;
pub use self::adapt::{AdaptCustomBodyLayer, AdaptCustomBodyService};
pub use self::adapt::{AdaptOuterBodyLayer, AdaptOuterBodyService};
