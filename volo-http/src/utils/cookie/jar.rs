//! Cookie utilities of Volo-HTTP.
//!
//! [`CookieJar`] currently only supports the server side.

use std::{convert::Infallible, ops::Deref};

use cookie::Cookie;
use http::{HeaderMap, header, request::Parts};

use crate::context::ServerContext;
#[cfg(feature = "server")]
use crate::server::extract::FromContext;

/// A cooke jar that can be extracted from a handler.
pub struct CookieJar {
    inner: cookie::CookieJar,
}

impl CookieJar {
    /// Create a [`CookieJar`] from given [`HeaderMap`]
    pub fn from_header(headers: &HeaderMap) -> Self {
        let mut jar = cookie::CookieJar::new();
        for cookie in headers
            .get_all(header::COOKIE)
            .into_iter()
            .filter_map(|val| val.to_str().ok())
            .flat_map(|val| val.split(';'))
            .filter_map(|cookie| Cookie::parse_encoded(cookie.to_owned()).ok())
        {
            jar.add_original(cookie);
        }

        Self { inner: jar }
    }
}

impl Deref for CookieJar {
    type Target = cookie::CookieJar;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[cfg(feature = "server")]
impl FromContext for CookieJar {
    type Rejection = Infallible;

    async fn from_context(
        _cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self::from_header(&parts.headers))
    }
}
