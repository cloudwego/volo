//! Constants of HTTP(S) protocol.

use http::header::HeaderValue;

/// Default port of HTTP server.
pub const HTTP_DEFAULT_PORT: u16 = 80;
/// Default port of HTTPS server.
pub const HTTPS_DEFAULT_PORT: u16 = 443;

/// `application/json`
pub const APPLICATION_JSON: HeaderValue = HeaderValue::from_static("application/json");
/// `application/x-www-form-urlencoded`
pub const APPLICATION_WWW_FORM_URLENCODED: HeaderValue =
    HeaderValue::from_static("application/x-www-form-urlencoded");
