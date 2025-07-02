use std::task::{Context, Poll};

use hyper::body::Incoming;
use volo::net::Address;

use crate::{
    body::{BoxBody, boxed},
    metadata::HEADER_TRANS_REMOTE_ADDR,
};

#[derive(Clone, Debug)]
pub struct IncomingService<S> {
    inner: S,
    peer_addr: Option<Address>,
}

impl<S> IncomingService<S> {
    pub fn new(inner: S, peer_addr: Option<Address>) -> Self {
        Self { inner, peer_addr }
    }
}

impl<S> tower::Service<hyper::Request<Incoming>> for IncomingService<S>
where
    S: tower::Service<hyper::Request<BoxBody>>,
{
    type Response = S::Response;

    type Error = S::Error;

    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: hyper::Request<Incoming>) -> Self::Future {
        if !req.headers().contains_key(HEADER_TRANS_REMOTE_ADDR) {
            if let Some(addr) = &self.peer_addr {
                if let Ok(addr) = addr.to_string().parse() {
                    req.headers_mut().insert(HEADER_TRANS_REMOTE_ADDR, addr);
                }
            }
        }

        self.inner.call(req.map(boxed))
    }
}
