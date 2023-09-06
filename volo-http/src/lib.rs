#![feature(impl_trait_in_assoc_type)]

pub(crate) mod dispatch;
pub mod extract;
pub mod handler;
pub mod layer;
pub mod param;
pub mod request;
pub mod response;
pub mod route;

use std::{future::Future, net::SocketAddr};

use http::{Extensions, HeaderMap, HeaderValue, Method, Uri, Version};
use hyper::{
    body::{Body, Incoming},
    Request, Response,
};
use param::Params;

pub type DynError = Box<dyn std::error::Error + Send + Sync>;

pub struct HttpContextInner {
    pub(crate) peer: SocketAddr,

    pub(crate) method: Method,
    pub(crate) uri: Uri,
    pub(crate) version: Version,
    pub(crate) headers: HeaderMap<HeaderValue>,
    pub(crate) extensions: Extensions,
}

pub struct HttpContext {
    pub peer: SocketAddr,
    pub method: Method,
    pub uri: Uri,
    pub version: Version,
    pub headers: HeaderMap<HeaderValue>,
    pub extensions: Extensions,

    pub params: Params,
}

#[derive(Clone)]
pub struct MotoreService<S> {
    peer: SocketAddr,
    inner: S,
}

impl<OB, S> hyper::service::Service<Request<Incoming>> for MotoreService<S>
where
    OB: Body<Error = DynError>,
    S: motore::Service<(), (HttpContextInner, Incoming), Response = Response<OB>> + Clone,
    S::Error: Into<DynError>,
{
    type Response = S::Response;

    type Error = S::Error;

    type Future = impl Future<Output = Result<Self::Response, Self::Error>>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let s = self.inner.clone();
        let peer = self.peer;
        async move {
            let (parts, req) = req.into_parts();
            let cx = HttpContextInner {
                peer,
                method: parts.method,
                uri: parts.uri,
                version: parts.version,
                headers: parts.headers,
                extensions: parts.extensions,
            };
            s.call(&mut (), (cx, req)).await
        }
    }
}
