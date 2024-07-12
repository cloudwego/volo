//! Response types for client and server.

use hyper::body::Incoming;

use crate::body::Body;

/// [`Response`][Response] with [`Body`] as default body
///
/// [Response]: http::Response
pub type ServerResponse<B = Body> = http::Response<B>;

/// [`Response`][Response] with [`Incoming`] as default body
///
/// [Response]: http::Response
pub type ClientResponse<B = Incoming> = http::Response<B>;
