//! A collection of utilities for working with `Service` types and Servers.

mod http;
mod make;
mod option;
mod retry;
mod serviceref;
mod shared;
mod timeout;

pub use self::http::HttpService;

pub use self::make::{make_service_fn, BoxMakeServiceLayer, BoxMakeServiceRef, MakeServiceRef};
pub use self::retry::{Attempts, Retry, RetryLayer};
pub use option::{OptionLayer, OptionLayerExt, OptionService};
pub use serviceref::ServiceRef;
pub use shared::SharedService;
pub use timeout::{Timeout, TimeoutLayer};
pub use tower::{service_fn, Service, ServiceBuilder, ServiceExt};
