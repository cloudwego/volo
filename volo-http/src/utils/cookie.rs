//! Cookie utilities of Volo-HTTP.
//!
//! [`CookieJar`] currently only supports the server side.

use std::{
    borrow::Cow,
    convert::Infallible,
    ops::{Deref, DerefMut},
};

use bytes::Bytes;
pub use cookie::{time::Duration, Cookie};
use http::{header, request::Parts, HeaderMap, HeaderValue};

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

    /// Create a [`CookieJar`] from given string
    ///
    /// # Example
    ///
    /// ```rust
    /// use volo_http::utils::cookie::CookieJar;
    /// let cookie_jar = CookieJar::from_cookie_str("foo=bar; ;foo1=bar1");
    /// ```
    #[cfg(feature = "client")]
    pub fn from_cookie_str<S>(s: S) -> Self
    where
        S: Into<Cow<'static, str>>,
    {
        let cookies: Vec<Cookie> = Cookie::split_parse(s)
            .filter_map(|parse| parse.ok())
            .collect();

        let mut jar = cookie::CookieJar::new();
        for cookie in cookies {
            jar.add_original(cookie);
        }

        Self { inner: jar }
    }

    #[cfg(feature = "client")]
    /// Create a empty [`CookieJar`]
    pub fn new() -> Self {
        Self {
            inner: cookie::CookieJar::new(),
        }
    }

    #[cfg(feature = "client")]
    /// Add a cookie to the cookie jar
    ///
    /// # Example
    ///
    /// ```rust
    /// use volo_http::utils::cookie::CookieJar;
    /// let mut cookie_jar = CookieJar::new();
    /// cookie_jar.add_original(("foo", "bar"));
    /// cookie_jar.add_original(("foo1", "bar1"))
    /// ```
    pub(crate) fn add_original<C>(&mut self, cookie: C)
    where
        C: Into<Cookie<'static>>,
    {
        self.inner.add_original(cookie);
    }

    /// Get [`HeaderValue`] from the cookie jar
    #[cfg(feature = "client")]
    pub(crate) fn cookies(&self) -> Option<HeaderValue> {
        let s = self
            .inner
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join("; ");

        if s.is_empty() {
            return None;
        }

        HeaderValue::from_maybe_shared(Bytes::from(s)).ok()
    }
}

impl Deref for CookieJar {
    type Target = cookie::CookieJar;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for CookieJar {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
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
