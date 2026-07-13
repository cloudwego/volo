//! Test cases for Client

#![allow(unused)]

use std::collections::HashMap;

use http::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Deserializer};

#[cfg(feature = "http1")]
mod http1_only;
#[cfg(feature = "__tls")]
mod tls;
mod utils;

const DEFAULT_HTTPBIN_BASE_URL: &str = "http://httpbin.org";
#[cfg(feature = "__tls")]
const DEFAULT_HTTPBIN_HTTPS_BASE_URL: &str = "https://httpbin.org";
const LOGID_KEY: HeaderName = HeaderName::from_static("x-log-id");
const LOGID_VAL: HeaderValue = HeaderValue::from_static("20201231114514");

#[derive(Clone, Debug)]
struct HttpBinEndpoint {
    base_url: String,
    host: String,
    authority: String,
    port: Option<u16>,
}

impl HttpBinEndpoint {
    fn from_env(env: &str, default: &str) -> Self {
        let base_url = std::env::var(env).unwrap_or_else(|_| default.to_owned());
        let base_url = base_url.trim_end_matches('/').to_owned();
        let uri = base_url
            .parse::<http::Uri>()
            .expect("invalid httpbin base URL");
        let host = uri.host().expect("httpbin base URL must include host");
        let authority = uri
            .authority()
            .expect("httpbin base URL must include authority");

        Self {
            base_url,
            host: host.to_owned(),
            authority: authority.as_str().to_owned(),
            port: uri.port_u16(),
        }
    }

    fn uri(&self) -> http::Uri {
        self.url("/").parse().expect("invalid httpbin URL")
    }

    fn port_or(&self, default: u16) -> u16 {
        self.port.unwrap_or(default)
    }

    fn url(&self, path: &str) -> String {
        debug_assert!(path.starts_with('/'));
        format!("{}{}", self.base_url, path)
    }
}

fn httpbin_endpoint() -> HttpBinEndpoint {
    HttpBinEndpoint::from_env("VOLO_HTTPBIN_BASE_URL", DEFAULT_HTTPBIN_BASE_URL)
}

fn httpbin_url(path: &str) -> String {
    httpbin_endpoint().url(path)
}

#[cfg(feature = "__tls")]
fn httpbin_https_endpoint() -> HttpBinEndpoint {
    HttpBinEndpoint::from_env(
        "VOLO_HTTPBIN_HTTPS_BASE_URL",
        DEFAULT_HTTPBIN_HTTPS_BASE_URL,
    )
}

#[cfg(feature = "__tls")]
fn httpbin_https_url(path: &str) -> String {
    httpbin_https_endpoint().url(path)
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StringOrStrings {
    String(String),
    Strings(Vec<String>),
}

impl StringOrStrings {
    fn into_string(self) -> String {
        match self {
            Self::String(s) => s,
            Self::Strings(values) => values.into_iter().next().unwrap_or_default(),
        }
    }
}

fn deserialize_string_map<'de, D>(deserializer: D) -> Result<HashMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = HashMap::<String, StringOrStrings>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .map(|(key, value)| (key, value.into_string()))
        .collect())
}

#[derive(Deserialize)]
struct HttpBinResponse {
    #[serde(default, deserialize_with = "deserialize_string_map")]
    args: HashMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_string_map")]
    headers: HashMap<String, String>,
    origin: String,
    url: String,
    #[serde(default, deserialize_with = "deserialize_string_map")]
    form: HashMap<String, String>,
    #[serde(default)]
    json: Option<HashMap<String, String>>,
}
