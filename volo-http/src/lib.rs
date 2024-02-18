pub mod body;
pub mod client;
pub mod context;
#[cfg(feature = "cookie")]
pub mod cookie;
pub mod error;
pub mod extension;
pub mod extract;
pub mod handler;
#[cfg(any(feature = "serde_json", feature = "sonic_json"))]
pub mod json;
pub mod layer;
pub mod middleware;
pub mod param;
pub mod request;
pub mod response;
pub mod route;
pub mod server;

pub(crate) mod service_fn;

mod macros;

#[doc(hidden)]
pub mod prelude {
    pub use bytes::Bytes;
    pub use http;
    pub use hyper;
    pub use volo::net::Address;

    #[cfg(feature = "cookie")]
    pub use crate::cookie::CookieJar;
    #[cfg(any(feature = "serde_json", feature = "sonic_json"))]
    pub use crate::json::Json;
    pub use crate::{extension::Extension, param::Params, route::Router, server::Server};
}

pub use self::prelude::*;
