//! A collection of utilities for working with `Service` types and Servers.

mod http;
mod make;

pub use self::http::HttpService;
pub use self::make::{make_service_fn, MakeServiceFn, MakeServiceRef};
pub use tower::service_fn;
