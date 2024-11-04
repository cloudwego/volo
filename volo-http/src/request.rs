//! Request types and utils.

use http::{
    header::{self, HeaderMap, HeaderName},
    request::{Parts, Request},
    uri::Scheme,
    Uri,
};

/// [`Request`] with [`Body`] as default body.
///
/// [`Body`]: crate::body::Body
#[cfg(feature = "client")]
pub type ClientRequest<B = crate::body::Body> = Request<B>;

/// [`Request`] with [`Incoming`] as default body.
///
/// [`Incoming`]: hyper::body::Incoming
#[cfg(feature = "server")]
pub type ServerRequest<B = crate::body::Body> = Request<B>;

/// HTTP header [`X-Forwarded-For`][mdn].
///
/// [mdn]: https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/X-Forwarded-For
pub const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");

/// HTTP header `X-Real-IP`.
pub const X_REAL_IP: HeaderName = HeaderName::from_static("x-real-ip");

/// Utilities of [`http::request::Parts`] and [`http::Request`].
pub trait RequestPartsExt: sealed::SealedRequestPartsExt {
    /// Get host name of the request URI from header `Host`.
    fn host(&self) -> Option<&str>;
    /// Get URL of the request URI.
    fn url(&self) -> Option<url::Url>;
}

mod sealed {
    pub trait SealedRequestPartsExt {
        fn headers(&self) -> &http::header::HeaderMap;
        fn uri(&self) -> &http::Uri;
        fn extensions(&self) -> &http::Extensions;
    }
}

impl sealed::SealedRequestPartsExt for Parts {
    fn headers(&self) -> &HeaderMap {
        &self.headers
    }
    fn uri(&self) -> &Uri {
        &self.uri
    }
    fn extensions(&self) -> &http::Extensions {
        &self.extensions
    }
}
impl<B> sealed::SealedRequestPartsExt for Request<B> {
    fn headers(&self) -> &HeaderMap {
        self.headers()
    }
    fn uri(&self) -> &Uri {
        self.uri()
    }
    fn extensions(&self) -> &http::Extensions {
        self.extensions()
    }
}

impl<T> RequestPartsExt for T
where
    T: sealed::SealedRequestPartsExt,
{
    fn host(&self) -> Option<&str> {
        simdutf8::basic::from_utf8(self.headers().get(header::HOST)?.as_bytes()).ok()
    }

    fn url(&self) -> Option<url::Url> {
        let host = self.host()?;
        let uri = self.uri();

        let mut url_str = String::new();

        if let Some(scheme) = self.extensions().get::<Scheme>() {
            url_str.push_str(scheme.as_str());
            url_str.push_str("://");
        } else {
            url_str.push_str("http://");
        }

        url_str.push_str(host);
        url_str.push_str(uri.path());

        url::Url::parse(url_str.as_str()).ok()
    }
}
