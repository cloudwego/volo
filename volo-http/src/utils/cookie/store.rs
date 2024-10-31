use std::ops::{Deref, DerefMut};

use bytes::Bytes;
use cookie::Cookie;
use http::{header, HeaderMap, HeaderValue};

#[derive(Default)]
pub struct CookieStore {
    inner: cookie_store::CookieStore,
}

impl CookieStore {
    pub fn new(cookie_store: cookie_store::CookieStore) -> Self {
        Self {
            inner: cookie_store,
        }
    }

    pub fn add_cookie_header(&self, headers: &mut HeaderMap, request_url: &url::Url) {
        if let Some(header_value) = self.cookies(request_url) {
            headers.insert(header::COOKIE, header_value);
        }
    }

    pub fn store_response_headers(&mut self, headers: &HeaderMap, request_url: &url::Url) {
        let mut set_cookie_headers = headers.get_all(header::SET_COOKIE).iter().peekable();

        if set_cookie_headers.peek().is_some() {
            let cookie_iter = set_cookie_headers.filter_map(|val| {
                std::str::from_utf8(val.as_bytes())
                    .ok()
                    .and_then(|val| Cookie::parse(val).map(|c| c.into_owned()).ok())
            });
            self.inner.store_response_cookies(cookie_iter, request_url);
        }
    }

    /// Get [`HeaderValue`] from the cookie store
    pub fn cookies(&self, request_url: &url::Url) -> Option<HeaderValue> {
        let mut cookie_iter = self.inner.get_request_values(request_url);

        let mut size = 0;

        for (key, value) in cookie_iter.by_ref() {
            size += key.len() + value.len() + 3;
        }

        let mut s = String::with_capacity(size);

        for (name, value) in cookie_iter {
            s.push_str(name);
            s.push('=');
            s.push_str(value);
            s.push_str("; ");
        }

        if s.is_empty() {
            return None;
        }

        HeaderValue::from_maybe_shared(Bytes::from(s)).ok()
    }
}

impl Deref for CookieStore {
    type Target = cookie_store::CookieStore;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for CookieStore {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
