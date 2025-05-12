//! Test cases for Client

#![allow(dead_code)]

use std::collections::HashMap;

use serde::Deserialize;

#[cfg(not(feature = "http2"))]
mod http1_only;
#[cfg(feature = "__tls")]
mod tls;

const HTTPBIN_GET: &str = "http://httpbin.org/get";
#[cfg(feature = "__tls")]
const HTTPBIN_GET_HTTPS: &str = "https://httpbin.org/get";
const USER_AGENT_KEY: &str = "User-Agent";
const USER_AGENT_VAL: &str = "volo-http-unit-test";

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
