//! Response types for client and server.

use crate::body::Body;

/// [`Response`] with [`Body`] as default body
///
/// [`Response`]: http::response::Response
pub type Response<B = Body> = http::response::Response<B>;
