//! Response types for client and server.

#[cfg(feature = "cookie")]
use cookie::Cookie;
use http::Response;

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

/// Utilities of [`http::response::Response`].
pub trait ResponseExt: sealed::SealedResponseExt {
    /// Get all cookies from `Set-Cookie` header.
    #[cfg(feature = "cookie")]
    fn cookies(&self) -> impl Iterator<Item = Cookie>;
}

mod sealed {
    pub trait SealedResponseExt {
        fn headers(&self) -> &http::HeaderMap;
    }
}

impl<B> sealed::SealedResponseExt for Response<B> {
    fn headers(&self) -> &http::HeaderMap {
        self.headers()
    }
}

impl<T> ResponseExt for T
where
    T: sealed::SealedResponseExt,
{
    #[cfg(feature = "cookie")]
    fn cookies(&self) -> impl Iterator<Item = Cookie> {
        self.headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|value| {
                std::str::from_utf8(value.as_bytes())
                    .ok()
                    .and_then(|val| Cookie::parse(val).map(|c| c.into_owned()).ok())
            })
    }
}
