pub mod context;
#[cfg(feature = "cookie")]
pub mod cookie;
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
pub mod tracing;

pub(crate) mod service_fn;

mod macros;

#[doc(hidden)]
pub mod prelude {
    pub use bytes::Bytes;
    pub use hyper::{
        self,
        body::Incoming as BodyIncoming,
        http::{self, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, Version},
    };
    pub use volo::net::Address;

    #[cfg(feature = "cookie")]
    pub use crate::cookie::CookieJar;
    #[cfg(any(feature = "serde_json", feature = "sonic_json"))]
    pub use crate::json::Json;
    pub use crate::{
        context::{ConnectionInfo, HttpContext, ServerContext},
        extension::Extension,
        extract::{Form, MaybeInvalid, Query, State},
        param::Params,
        request::Request,
        response::Response,
        route::Router,
        server::Server,
    };

    pub type DynService =
        motore::BoxCloneService<ServerContext, BodyIncoming, Response, std::convert::Infallible>;
}

pub use prelude::*;
