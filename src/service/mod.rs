//! A collection of utilities for working with `Service` types and Servers.

mod adapt;
mod http;
#[cfg(feature = "incoming")]
mod incoming;
mod make;
mod serviceref;
mod shared;

pub use self::adapt::{AdaptCustomBodyExt, AdaptCustomBodyLayer, AdaptCustomBodyService};
pub use self::adapt::{AdaptOuterBodyLayer, AdaptOuterBodyService};
pub use self::http::HttpService;
#[cfg(feature = "incoming")]
pub use self::incoming::{AdaptIncomingLayer, AdaptIncomingService};
pub use self::make::{make_service_fn, BoxMakeServiceLayer, BoxMakeServiceRef, MakeServiceRef};
pub use serviceref::ServiceRef;
pub use shared::SharedService;
pub use tower::{service_fn, Service, ServiceBuilder, ServiceExt};
