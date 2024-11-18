//! Response types for client and server.

use crate::body::Body;

/// [`Response`] with [`Body`] as default body
///
/// [`Response`]: http::response::Response
pub type Response<B = Body> = http::response::Response<B>;

/// [`Response`] with [`Body`] as default body
///
/// [`Response`]: http::response::Response
#[cfg(feature = "server")]
#[deprecated(note = "`ServerResponse` has been renamed to `Response`")]
pub type ServerResponse<B = Body> = Response<B>;

/// [`Response`] with [`Body`] as default body
///
/// [`Response`]: http::response::Response
#[cfg(feature = "client")]
#[deprecated(note = "`ClientResponse` has been renamed to `Response`")]
pub type ClientResponse<B = Body> = Response<B>;
