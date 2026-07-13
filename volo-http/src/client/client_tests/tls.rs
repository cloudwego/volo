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

use super::{HttpBinResponse, httpbin_https_endpoint, httpbin_https_url};
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
    let mut builder = Client::builder();
    if let Ok(cert_path) = std::env::var("VOLO_HTTPBIN_CA_CERT") {
        let tls_connector = volo::net::tls::TlsConnector::builder()
            .add_pem_from_file(cert_path)
            .expect("failed to load httpbin CA certificate")
            .build()
            .expect("failed to build httpbin TLS connector");
        builder.set_tls_config(tls_connector);
    }
    builder
        .layer_inner(RetryOnStatus::server_error())
        .layer_inner_front(DebugLayer::default())
}

#[cfg(feature = "json")]
#[tokio::test]
async fn simple_https_request() {
    let httpbin_get = httpbin_https_url("/get");
    let client = builder_for_debug().build().unwrap();
    let resp = client
        .get(&httpbin_get)
        .send()
        .await
        .unwrap()
        .into_json::<HttpBinResponse>()
        .await
        .unwrap();
    assert!(resp.args.is_empty());
    assert_eq!(resp.url, httpbin_get);
}

#[cfg(feature = "json")]
#[tokio::test]
async fn client_builder_with_https() {
    let httpbin = httpbin_https_endpoint();
    let httpbin_get = httpbin.url("/get");
    let client = builder_for_debug()
        .layer_outer_front(TargetLayer::new(
            Target::new_host(
                Some(http::uri::Scheme::HTTPS),
                httpbin.host.clone(),
                Some(httpbin.port_or(crate::utils::consts::HTTPS_DEFAULT_PORT)),
            )
            .unwrap(),
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
    assert_eq!(resp.url, httpbin_get);
}

#[cfg(feature = "json")]
#[tokio::test]
async fn client_builder_with_address_and_https() {
    let httpbin = httpbin_https_endpoint();
    let httpbin_get = httpbin.url("/get");
    let ip = match httpbin.host.parse() {
        Ok(ip) => ip,
        Err(_) => DnsResolver::default()
            .resolve(httpbin.host.as_str())
            .await
            .unwrap(),
    };
    let mut target = Target::from(volo::net::Address::Ip(std::net::SocketAddr::new(
        ip,
        httpbin.port_or(crate::utils::consts::HTTPS_DEFAULT_PORT),
    )));
    target.set_scheme(http::uri::Scheme::HTTPS);
    let mut builder = builder_for_debug()
        .layer_outer_front(TargetLayer::new(target).with_service_name(httpbin.host.clone()));
    builder.header(http::header::HOST, httpbin.authority.clone());
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
    assert_eq!(resp.url, httpbin_get);
}

#[tokio::test]
async fn client_disable_tls() {
    use crate::error::client::bad_scheme;

    let mut builder = Client::builder()
        .layer_inner(RetryOnStatus::server_error())
        .layer_inner_front(DebugLayer::default());
    builder.disable_tls(true);
    let client = builder.build().unwrap();
    assert!(
        client
            .get(httpbin_https_url("/get"))
            .send()
            .await
            .expect_err("HTTPS with disable_tls should fail")
            .source()
            .expect("HTTPS with disable_tls should fail")
            .is::<BadScheme>()
    );
}
