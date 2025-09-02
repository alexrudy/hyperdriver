//! A collection of utilities for working with `Service` types and Servers.

mod error;
#[cfg(feature = "client")]
mod host;
mod http;
#[cfg(feature = "incoming")]
mod incoming;
mod option;
mod timeout;

pub use self::error::{MaybeErrorFuture, PreprocessLayer, PreprocessService};
#[cfg(feature = "client")]
pub use self::host::{SetHostHeader, SetHostHeaderLayer};
#[cfg(feature = "client")]
pub use self::http::http1::{Http1ChecksLayer, Http1ChecksService};
#[cfg(feature = "client")]
pub use self::http::http2::{Http2ChecksLayer, Http2ChecksService};
pub use self::http::HttpService;
#[cfg(feature = "incoming")]
#[deprecated(since = "0.7.0", note = "Use IncomingRequestService instead")]
pub use self::incoming::{
    IncomingRequestLayer as AdaptIncomingLayer, IncomingRequestService as AdaptIncomingService,
};
#[cfg(feature = "incoming")]
pub use self::incoming::{
    IncomingRequestLayer, IncomingRequestService, IncomingResponseLayer, IncomingResponseService,
};

pub use option::{OptionLayer, OptionLayerExt, OptionService};
pub use timeout::{Timeout, TimeoutLayer};
pub use tower::{service_fn, Service, ServiceBuilder, ServiceExt};
