pub(crate) mod dispatch;
pub mod extract;
pub mod handler;
pub mod layer;
pub mod param;
pub mod request;
pub mod response;
pub mod route;
pub mod server;

use http::{Extensions, HeaderMap, HeaderValue, Method, Uri, Version};
use param::Params;
use volo::net::Address;

pub type DynError = Box<dyn std::error::Error + Send + Sync>;

pub struct HttpContext {
    pub peer: Address,
    pub method: Method,
    pub uri: Uri,
    pub version: Version,
    pub headers: HeaderMap<HeaderValue>,
    pub extensions: Extensions,

    pub params: Params,
}
