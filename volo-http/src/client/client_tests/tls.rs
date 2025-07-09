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

use motore::layer::{Identity, Stack};

use super::{HTTPBIN_GET_HTTPS, HttpBinResponse};
use crate::{
    ClientBuilder,
    body::BodyConversion,
    client::{
        Client, Target,
        dns::DnsResolver,
        layer::TargetLayer,
        test_helpers::{DebugLayer, RetryOnStatus},
    },
    error::client::BadScheme,
};

fn builder_for_debug() -> ClientBuilder<Stack<Stack<RetryOnStatus, Identity>, DebugLayer>, Identity>
{
    Client::builder()
        .layer_inner(RetryOnStatus::server_error())
        .layer_inner_front(DebugLayer::default())
}

#[cfg(feature = "json")]
#[tokio::test]
async fn simple_https_request() {
    let client = builder_for_debug().build().unwrap();
    let resp = client
        .get(HTTPBIN_GET_HTTPS)
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
async fn client_builder_with_https() {
    let client = builder_for_debug()
        .layer_outer_front(TargetLayer::new(
            Target::new_host(Some(http::uri::Scheme::HTTPS), "httpbin.org", None).unwrap(),
        ))
        .build()
        .unwrap();

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
    let ip = DnsResolver::default().resolve("httpbin.org").await.unwrap();
    let mut target = Target::from(volo::net::Address::Ip(std::net::SocketAddr::new(
        ip,
        crate::utils::consts::HTTPS_DEFAULT_PORT,
    )));
    target.set_scheme(http::uri::Scheme::HTTPS);
    let mut builder = builder_for_debug()
        .layer_outer_front(TargetLayer::new(target).with_service_name("httpbin.org"));
    builder.header(http::header::HOST, "httpbin.org");
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

    let mut builder = builder_for_debug();
    builder.disable_tls(true);
    let client = builder.build().unwrap();
    assert!(
        client
            .get("https://httpbin.org/get")
            .send()
            .await
            .expect_err("HTTPS with disable_tls should fail")
            .source()
            .expect("HTTPS with disable_tls should fail")
            .is::<BadScheme>()
    );
}
