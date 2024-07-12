//! Request types for client and server.

use hyper::body::Incoming;

use crate::body::Body;

/// [`Request`][Request] with [`Body`] as default body
///
/// [Request]: http::Request
pub type ClientRequest<B = Body> = http::Request<B>;

/// [`Request`][Request] with [`Incoming`] as default body
///
/// [Request]: http::Request
pub type ServerRequest<B = Incoming> = http::Request<B>;
