//! A collection of utilities for working with `Service` types and Servers.

#[cfg(feature = "client")]
pub(crate) mod client;
mod error;
#[cfg(feature = "client")]
mod host;
mod http;
#[cfg(feature = "incoming")]
mod incoming;
mod make;
mod option;
mod serviceref;
mod shared;
mod timeout;

#[cfg(feature = "client")]
pub use self::client::{ExecuteRequest, RequestExecutor};
pub use self::error::{MaybeErrorFuture, PreprocessLayer, PreprocessService};
#[cfg(feature = "client")]
pub use self::host::{SetHostHeader, SetHostHeaderLayer};
#[cfg(feature = "client")]
pub use self::http::http1::{Http1ChecksLayer, Http1ChecksService};
#[cfg(feature = "client")]
pub use self::http::http2::{Http2ChecksLayer, Http2ChecksService};
pub use self::http::HttpService;
#[cfg(feature = "incoming")]
pub use self::incoming::{
    IncomingRequestLayer, IncomingRequestService, IncomingResponseLayer, IncomingResponseService,
};
pub use self::make::{make_service_fn, BoxMakeServiceLayer, BoxMakeServiceRef, MakeServiceRef};
pub use option::{OptionLayer, OptionLayerExt, OptionService};
pub use serviceref::ServiceRef;
pub use shared::SharedService;
pub use timeout::{Timeout, TimeoutLayer};
pub use tower::{service_fn, Service, ServiceBuilder, ServiceExt};
