use std::task::{Context, Poll};

use volo::net::Address;

use crate::{
    body::{BoxBody, boxed},
    metadata::HEADER_TRANS_REMOTE_ADDR,
};

#[derive(Clone, Debug)]
pub struct IncomingService<S> {
    inner: S,
    peer_addr: Option<Address>,
    strict_grpc: bool,
}

impl<S> IncomingService<S> {
    pub fn new(inner: S, peer_addr: Option<Address>) -> Self {
        Self {
            inner,
            peer_addr,
            strict_grpc: false,
        }
    }

    pub(super) fn with_strict_grpc(mut self) -> Self {
        self.strict_grpc = true;
        self
    }
}

impl<S, B> tower::Service<hyper::Request<B>> for IncomingService<S>
where
    S: tower::Service<hyper::Request<BoxBody>>,
    B: http_body::Body<Data = bytes::Bytes> + Send + 'static,
    B::Error: Into<crate::BoxError>,
{
    type Response = S::Response;

    type Error = S::Error;

    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: hyper::Request<B>) -> Self::Future {
        if self.strict_grpc {
            req.extensions_mut().insert(super::policy::StrictGrpc);
        }
        if !req.headers().contains_key(HEADER_TRANS_REMOTE_ADDR)
            && let Some(addr) = &self.peer_addr
            && let Ok(addr) = addr.to_string().parse()
        {
            req.headers_mut().insert(HEADER_TRANS_REMOTE_ADDR, addr);
        }

        self.inner.call(req.map(boxed))
    }
}
