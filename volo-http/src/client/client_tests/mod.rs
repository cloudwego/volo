//! Test cases for Client

#![allow(unused)]

use std::collections::HashMap;

use http::header::{HeaderName, HeaderValue};
use serde::Deserialize;

#[cfg(feature = "http1")]
mod http1_only;
#[cfg(feature = "__tls")]
mod tls;
mod utils;

const HTTPBIN_GET: &str = "http://httpbin.org/get";
const HTTPBIN_POST: &str = "http://httpbin.org/post";
#[cfg(feature = "__tls")]
const HTTPBIN_GET_HTTPS: &str = "https://httpbin.org/get";
const LOGID_KEY: HeaderName = HeaderName::from_static("x-log-id");
const LOGID_VAL: HeaderValue = HeaderValue::from_static("20201231114514");

#[derive(Deserialize)]
struct HttpBinResponse {
    args: HashMap<String, String>,
    headers: HashMap<String, String>,
    origin: String,
    url: String,
    #[serde(default)]
    form: HashMap<String, String>,
    #[serde(default)]
    json: Option<HashMap<String, String>>,
}
