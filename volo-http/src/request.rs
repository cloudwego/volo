use std::ops::{Deref, DerefMut};

use hyper::{body::Incoming, http::request::Builder};

pub struct Request(pub(crate) hyper::http::Request<hyper::body::Incoming>);

impl Request {
    pub fn builder() -> Builder {
        Builder::new()
    }
}

impl Deref for Request {
    type Target = hyper::http::Request<hyper::body::Incoming>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Request {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<hyper::http::Request<Incoming>> for Request {
    fn from(value: hyper::http::Request<Incoming>) -> Self {
        Self(value)
    }
}
