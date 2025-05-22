// TODO:
//
// `rustls` supports setting alpn in `ClientConfig` or when creating `ClientConnection`, but
// `tokio-rustls` only supports setting it in `ClientConfig`.
//
// In other words, `tokio-rustls` does not support setting different alpn for each connection.
//
// Due to the above reasons, we only support using HTTP/2 with TLS when `http1`, `http2`, and
// `tls` features are enabled.

use std::error::Error;

use super::{HttpBinResponse, HTTPBIN_GET_HTTPS};
use crate::{
    body::BodyConversion,
    client::{dns::DnsResolver, test_helpers::DebugLayer, Client},
    error::client::BadScheme,
};

#[cfg(feature = "json")]
#[tokio::test]
async fn client_builder_with_https() {
    let mut builder = Client::builder().layer_inner(DebugLayer::default());
    builder
        .host("httpbin.org")
        .with_scheme(http::uri::Scheme::HTTPS);
    let client = builder.build().unwrap();

    let resp = client
        .get("/get")
        .send()
        .await
        .unwrap()
        .into_json::<HttpBinResponse>()
        .await
        .unwrap();
    assert!(resp.args.is_empty());
    assert_eq!(resp.url, HTTPBIN_GET_HTTPS);
}

#[cfg(feature = "json")]
#[tokio::test]
async fn client_builder_with_address_and_https() {
    let addr = DnsResolver::default()
        .resolve("httpbin.org", crate::utils::consts::HTTPS_DEFAULT_PORT)
        .await
        .unwrap();
    let mut builder = Client::builder().layer_inner(DebugLayer::default());
    builder
        .default_host("httpbin.org")
        .address(addr)
        .with_scheme(http::uri::Scheme::HTTPS);
    let client = builder.build().unwrap();

    let resp = client
        .get("/get")
        .send()
        .await
        .unwrap()
        .into_json::<HttpBinResponse>()
        .await
        .unwrap();
    assert!(resp.args.is_empty());
    assert_eq!(resp.url, HTTPBIN_GET_HTTPS);
}

#[tokio::test]
async fn client_disable_tls() {
    use crate::error::client::bad_scheme;

    let mut builder = Client::builder().layer_inner(DebugLayer::default());
    builder.disable_tls(true);
    let client = builder.build().unwrap();
    assert!(client
        .get("https://httpbin.org/get")
        .send()
        .await
        .expect_err("HTTPS with disable_tls should fail")
        .source()
        .expect("HTTPS with disable_tls should fail")
        .is::<BadScheme>());
}
