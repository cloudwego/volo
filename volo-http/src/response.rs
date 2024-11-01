//! Response types for client and server.

/// [`Response`] with [`Body`] as default body
///
/// [`Response`]: http::response::Response
/// [`Body`]: crate::body::Body
#[cfg(feature = "server")]
pub type ServerResponse<B = crate::body::Body> = http::response::Response<B>;

/// [`Response`] with [`Body`] as default body
///
/// [`Response`]: http::response::Response
/// [`Body`]: crate::body::Body
#[cfg(feature = "client")]
pub type ClientResponse<B = crate::body::Body> = http::response::Response<B>;
