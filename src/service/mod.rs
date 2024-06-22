//! A collection of utilities for working with `Service` types and Servers.

#[cfg(feature = "server")]
mod http;
#[cfg(feature = "server")]
mod make;
mod serviceref;

pub use self::adapt::{AdaptCustomBodyExt, AdaptCustomBodyLayer, AdaptCustomBodyService};
pub use self::adapt::{AdaptOuterBodyLayer, AdaptOuterBodyService};
#[cfg(feature = "server")]
pub use self::http::HttpService;
#[cfg(feature = "incoming")]
pub use self::incoming::{AdaptIncomingLayer, AdaptIncomingService};
#[cfg(feature = "server")]
pub use self::make::{make_service_fn, MakeServiceRef};
pub use serviceref::ServiceRef;
pub use tower::{service_fn, Service, ServiceBuilder, ServiceExt};
mod adapt;

#[cfg(feature = "incoming")]
mod incoming;
