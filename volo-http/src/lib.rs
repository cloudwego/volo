pub mod context;
#[cfg(feature = "cookie")]
pub mod cookie;
pub mod extension;
pub mod extract;
pub mod handler;
pub mod layer;
pub mod middleware;
pub mod param;
pub mod request;
pub mod response;
pub mod route;
pub mod server;

mod macros;

use std::convert::Infallible;

pub use bytes::Bytes;
#[cfg(feature = "cookie")]
pub use hyper::{
    self,
    body::Incoming as BodyIncoming,
    http::{self, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, Version},
};
pub use volo::net::Address;

#[cfg(feature = "cookie")]
pub use crate::cookie::CookieJar;
pub use crate::{
    context::{ConnectionInfo, HttpContext},
    extension::Extension,
    extract::{Form, Json, MaybeInvalid, Query, State},
    param::Params,
    request::Request,
    response::Response,
    route::Router,
    server::Server,
};

pub type DynService = motore::BoxCloneService<HttpContext, BodyIncoming, Response, Infallible>;
