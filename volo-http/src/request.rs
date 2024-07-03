//! Request types for client and server.
use hyper::body::Incoming;

use crate::body::Body;

pub type ClientRequest<B = Body> = http::Request<B>;
pub type ServerRequest<B = Incoming> = http::Request<B>;
