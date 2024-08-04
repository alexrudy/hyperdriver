//! A collection of utilities for working with `Service` types and Servers.

mod http;
#[cfg(feature = "incoming")]
mod incoming;
mod make;
mod option;
#[cfg(feature = "client")]
mod retry;
mod serviceref;
mod shared;
mod timeout;

pub use self::http::HttpService;
#[cfg(feature = "incoming")]
pub use self::incoming::{AdaptIncomingLayer, AdaptIncomingService};
pub use self::make::{make_service_fn, BoxMakeServiceLayer, BoxMakeServiceRef, MakeServiceRef};
#[cfg(feature = "client")]
pub use self::retry::{Attempts, Retry, RetryLayer};
pub use option::{OptionLayer, OptionLayerExt, OptionService};
pub use serviceref::ServiceRef;
pub use shared::SharedService;
pub use timeout::{Timeout, TimeoutLayer};
pub use tower::{service_fn, Service, ServiceBuilder, ServiceExt};
