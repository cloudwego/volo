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
    param::Params,
    request::{Json, Request},
    response::Response,
    server::Server,
};

pub type DynService = motore::BoxCloneService<HttpContext, Incoming, Response, Infallible>;

#[derive(Debug, Default, Clone, Copy)]
pub struct State<S>(pub S);

pub struct HttpContext {
    pub peer: Address,
    pub method: Method,
    pub uri: Uri,
    pub version: Version,
    pub headers: HeaderMap<HeaderValue>,
    pub extensions: Extensions,

    pub params: Params,
}
