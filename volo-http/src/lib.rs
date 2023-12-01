pub mod extract;
pub mod handler;
pub mod layer;
pub mod param;
pub mod request;
pub mod response;
pub mod route;
pub mod server;

mod macros;

pub use bytes::Bytes;
use http::{Extensions, HeaderMap, HeaderValue, Version};
pub use http::{Method, StatusCode, Uri};
use hyper::{body::Incoming, Response};
pub use volo::net::Address;

pub use crate::{param::Params, request::Json, server::Server};

mod private {
    #[derive(Debug, Clone, Copy)]
    pub enum ViaContext {}

    #[derive(Debug, Clone, Copy)]
    pub enum ViaRequest {}
}

pub type DynService =
    motore::BoxCloneService<HttpContext, Incoming, Response<response::RespBody>, DynError>;
pub type DynError = Box<dyn std::error::Error + Send + Sync>;

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
