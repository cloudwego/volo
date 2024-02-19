use hyper::body::Incoming;

use crate::body::Body;

pub type ServerResponse<B = Body> = http::Response<B>;
pub type ClientResponse<B = Incoming> = http::Response<B>;
