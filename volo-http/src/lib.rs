#![feature(impl_trait_in_assoc_type)]

pub(crate) mod dispatch;
<<<<<<< HEAD
<<<<<<< HEAD
pub mod extract;
pub mod handler;
=======
>>>>>>> init
=======
pub mod extract;
pub mod handler;
>>>>>>> handler, extractor (#221)
pub mod layer;
pub mod param;
pub mod request;
pub mod response;
pub mod route;
pub mod server;

use std::future::Future;

use http::{Extensions, HeaderMap, HeaderValue, Method, Uri, Version};
use hyper::{
    body::{Body, Incoming},
    Request, Response,
};
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

#[derive(Clone)]
pub struct MotoreService<S> {
    pub peer: Address,
    pub inner: S,
}

impl<OB, S> hyper::service::Service<Request<Incoming>> for MotoreService<S>
where
    OB: Body<Error = DynError>,
    S: motore::Service<HttpContext, Incoming, Response = Response<OB>> + Clone,
    S::Error: Into<DynError>,
{
    type Response = S::Response;

    type Error = S::Error;

    type Future = impl Future<Output = Result<Self::Response, Self::Error>>;

    fn call(&mut self, req: Request<Incoming>) -> Self::Future {
        let s = self.inner.clone();
        let peer = self.peer.clone();
        async move {
            let (parts, req) = req.into_parts();
            let mut cx = HttpContext {
                peer,
                method: parts.method,
                uri: parts.uri,
                version: parts.version,
                headers: parts.headers,
                extensions: parts.extensions,
                params: Params { inner: Vec::with_capacity(0) },
            };
            s.call(&mut cx, req).await
        }
    }
}
