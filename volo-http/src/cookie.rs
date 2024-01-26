use std::{convert::Infallible, ops::Deref};

pub use cookie::{time::Duration, Cookie};
use hyper::http::{header, HeaderMap};

use crate::{context::ServerContext, extract::FromContext};

pub struct CookieJar {
    inner: cookie::CookieJar,
}

impl CookieJar {
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

impl<S: Sync> FromContext<S> for CookieJar {
    type Rejection = Infallible;

    async fn from_context(cx: &mut ServerContext, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::from_header(cx.headers()))
    }
}
