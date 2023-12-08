pub mod context;
pub mod extract;
pub mod handler;
pub mod layer;
pub mod param;
pub mod request;
pub mod response;
pub mod route;
pub mod server;

mod macros;

use std::convert::Infallible;

pub use bytes::Bytes;
pub use hyper::{
    body::Incoming,
    http::{Extensions, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, Version},
};
pub use volo::net::Address;

pub use crate::{
    context::{ConnectionInfo, HttpContext},
    extract::{Json, MaybeInvalid, State},
    param::Params,
    request::Request,
    response::Response,
    server::Server,
};

pub type DynService = motore::BoxCloneService<HttpContext, Incoming, Response, Infallible>;
