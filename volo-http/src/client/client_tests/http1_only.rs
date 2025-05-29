// FIXME:
//
// `httpbin.org` supports h2 (HTTP/2 with tls), but doesn't support h2c (HTTP/2 over Cleartext),
// just disable those test cases.
//
// TODO:
//
// Find a website that support h2c.

use std::{
    any::TypeId,
    collections::HashMap,
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use bytes::Bytes;
use http::{header, status::StatusCode};
use http_body_util::Full;
use motore::service::Service;
use volo::context::Context;

use super::{
    utils::{
        AutoBody, AutoBodyLayer, AutoFull, AutoFullLayer, DropBodyLayer, Nothing,
        RespBodyToFullLayer,
    },
    HttpBinResponse, HTTPBIN_GET, HTTPBIN_POST, USER_AGENT_KEY, USER_AGENT_VAL,
};
use crate::{
    body::{Body, BodyConversion},
    client::{
        dns::DnsResolver,
        get,
        layer::FailOnStatus,
        test_helpers::{DebugLayer, MockTransport},
        CallOpt, Client,
    },
    context::client::Config,
    error::ClientError,
    response::Response,
    utils::consts::HTTP_DEFAULT_PORT,
};

#[tokio::test]
async fn client_with_generics() {
    fn type_of<T: 'static>(_: &T) -> TypeId {
        TypeId::of::<T>()
    }

    // Override default `ReqBody`, but the `ReqBody` is still implements `http_body::Body`
    {
        let client = Client::builder().build().unwrap();
        assert!(client
            .post(HTTPBIN_POST)
            .body(Full::new(Bytes::new()))
            .send()
            .await
            .is_ok());
        assert_eq!(TypeId::of::<Client<Full<Bytes>>>(), type_of(&client),);
    }
    // Override default `RespBody`, but the `RespBody` is still implements `http_body::Body`
    {
        let client = Client::builder()
            .layer_outer_front(RespBodyToFullLayer)
            .build()
            .unwrap();
        assert!(client.get(HTTPBIN_GET).send().await.is_ok());
        assert_eq!(TypeId::of::<Client<Body, Full<Bytes>>>(), type_of(&client),);
    }
    // Override default `ReqBody` through `Layer`. The `AutoBody` does not implement
    // `http_body::Body`, but the `AutoBodyLayer` will convert it to `volo_http::body::Body` and
    // use it.
    {
        let client = Client::builder()
            .layer_outer_front(AutoBodyLayer)
            .build()
            .unwrap();
        assert!(client
            .post(HTTPBIN_POST)
            .body(AutoBody)
            .send()
            .await
            .is_ok());
        assert_eq!(TypeId::of::<Client<AutoBody>>(), type_of(&client),);
    }
    // Override default `ReqBody` through `Layer`. The `AutoFull` does not implement
    // `http_body::Body`, but the `AutoFullLayer` will convert it to `Full<Bytes>` which implements
    // `http_body::Body` as its `InnerReqBody`.
    {
        let client = Client::builder()
            .layer_outer_front(AutoFullLayer)
            .build()
            .unwrap();
        assert!(client
            .post(HTTPBIN_POST)
            .body(AutoFull)
            .send()
            .await
            .is_ok());
        assert_eq!(TypeId::of::<Client<AutoFull>>(), type_of(&client),);
    }
    // Override default `RespBody` through `Layer`. The `RespBody` does not implement
    // `http_body::Body`, but the `DropBodyLayer` will drop `volo_http::body::Body` and put
    // `Nothing` to `Response`.
    {
        let client = Client::builder()
            .layer_outer_front(DropBodyLayer)
            .build()
            .unwrap();
        assert!(client.get(HTTPBIN_GET).send().await.is_ok());
        assert_eq!(TypeId::of::<Client<Body, Nothing>>(), type_of(&client),);
    }
    // Combine them
    {
        let client = Client::builder()
            .layer_outer_front(AutoFullLayer)
            .layer_outer_front(DropBodyLayer)
            .build()
            .unwrap();
        assert!(client.post(HTTPBIN_GET).body(AutoFull).send().await.is_ok());
        assert_eq!(TypeId::of::<Client<AutoFull, Nothing>>(), type_of(&client),);
    }
}

#[cfg(feature = "json")]
#[tokio::test]
async fn simple_get() {
    let resp = get(HTTPBIN_GET)
        .await
        .unwrap()
        .into_json::<HttpBinResponse>()
        .await
        .unwrap();
    assert!(resp.args.is_empty());
    assert_eq!(resp.url, HTTPBIN_GET);
}

#[cfg(feature = "json")]
#[tokio::test]
async fn client_builder_with_header() {
    let mut builder = Client::builder().layer_inner(DebugLayer::default());
    builder.header(header::USER_AGENT, USER_AGENT_VAL);
    let client = builder.build().unwrap();

    let resp = client
        .get(HTTPBIN_GET)
        .send()
        .await
        .unwrap()
        .into_json::<HttpBinResponse>()
        .await
        .unwrap();
    assert!(resp.args.is_empty());
    assert_eq!(resp.headers.get(USER_AGENT_KEY).unwrap(), USER_AGENT_VAL);
    assert_eq!(resp.url, HTTPBIN_GET);
}

#[cfg(feature = "json")]
#[tokio::test]
async fn client_builder_with_host() {
    let mut builder = Client::builder().layer_inner(DebugLayer::default());
    builder.host("httpbin.org");
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
    assert_eq!(resp.url, HTTPBIN_GET);
}

#[cfg(feature = "json")]
#[tokio::test]
async fn client_builder_with_address() {
    let addr = DnsResolver::default()
        .resolve("httpbin.org", HTTP_DEFAULT_PORT)
        .await
        .unwrap();
    let mut builder = Client::builder().layer_inner(DebugLayer::default());
    builder.default_host("httpbin.org").address(addr);
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
    assert_eq!(resp.url, HTTPBIN_GET);
}

#[cfg(feature = "json")]
#[tokio::test]
async fn client_builder_host_override() {
    let mut builder = Client::builder().layer_inner(DebugLayer::default());
    builder.host("this.domain.must.be.invalid");
    let client = builder.build().unwrap();

    let resp = client
        .get(HTTPBIN_GET)
        .send()
        .await
        .unwrap()
        .into_json::<HttpBinResponse>()
        .await
        .unwrap();
    assert!(resp.args.is_empty());
    assert_eq!(resp.url, HTTPBIN_GET);
}

#[cfg(feature = "json")]
#[tokio::test]
async fn client_builder_addr_override() {
    let mut builder = Client::builder().layer_inner(DebugLayer::default());
    builder.default_host("httpbin.org").address(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        8888,
    ));
    let client = builder.build().unwrap();

    let addr = DnsResolver::default()
        .resolve("httpbin.org", HTTP_DEFAULT_PORT)
        .await
        .unwrap();

    let resp = client
        .get(format!("http://{addr}/get"))
        .send()
        .await
        .unwrap()
        .into_json::<HttpBinResponse>()
        .await
        .unwrap();
    assert!(resp.args.is_empty());
    assert_eq!(resp.url, HTTPBIN_GET);
}

#[tokio::test]
async fn client_builder_with_port() {
    let mut builder = Client::builder().layer_inner(DebugLayer::default());
    builder.host("httpbin.org").with_port(443);
    let client = builder.build().unwrap();

    let resp = client.get("/get").send().await.unwrap();
    // Send HTTP request to the HTTPS port (443), `httpbin.org` will response `400 Bad
    // Request`.
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

fn test_data() -> HashMap<String, String> {
    HashMap::from([
        ("key1".to_string(), "val1".to_string()),
        ("key2".to_string(), "val2".to_string()),
    ])
}

#[cfg(all(feature = "query", feature = "json"))]
#[tokio::test]
async fn set_query() {
    let data = test_data();

    let client = Client::builder().build().unwrap();
    let resp = client
        .get("http://httpbin.org/get")
        .set_query(&data)
        .send()
        .await
        .unwrap()
        .into_json::<HttpBinResponse>()
        .await
        .unwrap();
    assert_eq!(resp.args, data);
}

#[cfg(all(feature = "form", feature = "json"))]
#[tokio::test]
async fn set_form() {
    let data = test_data();

    let client = Client::builder().build().unwrap();
    let resp = client
        .post("http://httpbin.org/post")
        .form(&data)
        .send()
        .await
        .unwrap()
        .into_json::<HttpBinResponse>()
        .await
        .unwrap();
    assert_eq!(resp.form, data);
}

#[cfg(feature = "json")]
#[tokio::test]
async fn set_json() {
    let data = test_data();

    let client = Client::builder().build().unwrap();
    let resp = client
        .post("http://httpbin.org/post")
        .json(&data)
        .send()
        .await
        .unwrap()
        .into_json::<HttpBinResponse>()
        .await
        .unwrap();
    assert_eq!(resp.json, Some(data));
}

struct GetTimeoutAsSeconds;

impl<Cx, Req> Service<Cx, Req> for GetTimeoutAsSeconds
where
    Cx: Context<Config = Config>,
{
    type Response = Response;
    type Error = ClientError;

    fn call(
        &self,
        cx: &mut Cx,
        _: Req,
    ) -> impl Future<Output = Result<Self::Response, Self::Error>> + Send {
        let timeout = cx.rpc_info().config().timeout();
        let resp = match timeout {
            Some(timeout) => {
                let secs = timeout.as_secs();
                Response::new(Body::from(format!("{secs}")))
            }
            None => {
                let mut resp = Response::new(Body::empty());
                *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                resp
            }
        };
        async { Ok(resp) }
    }
}

#[tokio::test]
async fn callopt_test() {
    let mut builder = Client::builder();
    builder.set_request_timeout(Duration::from_secs(1));
    let client = builder
        .layer_outer_front(FailOnStatus::server_error())
        .mock(MockTransport::service(GetTimeoutAsSeconds))
        .unwrap();
    // default timeout is 1 seconds
    assert_eq!(
        client
            .get("/")
            .send()
            .await
            .unwrap()
            .into_string()
            .await
            .unwrap(),
        "1"
    );
    // callopt set timeout to 5 seconds
    assert_eq!(
        client
            .get("/")
            .with_callopt(CallOpt::new().with_timeout(Duration::from_secs(5)))
            .send()
            .await
            .unwrap()
            .into_string()
            .await
            .unwrap(),
        "5"
    );
}

#[cfg(all(feature = "cookie", feature = "json"))]
#[tokio::test]
async fn cookie_store() {
    let mut builder = Client::builder()
        .layer_inner(DebugLayer::default())
        .layer_inner(crate::client::cookie::CookieLayer::new(Default::default()));

    builder.host("httpbin.org");

    let client = builder.build().unwrap();

    // test server add cookie
    let resp = client
        .get("http://httpbin.org/cookies/set?key=value")
        .send()
        .await
        .unwrap();
    let cookies = resp
        .headers()
        .get_all(http::header::SET_COOKIE)
        .iter()
        .filter_map(|value| {
            std::str::from_utf8(value.as_bytes())
                .ok()
                .and_then(|val| cookie::Cookie::parse(val).map(|c| c.into_owned()).ok())
        })
        .collect::<Vec<_>>();
    assert_eq!(cookies[0].name(), "key");
    assert_eq!(cookies[0].value(), "value");

    #[derive(serde::Deserialize)]
    struct CookieResponse {
        #[serde(default)]
        cookies: HashMap<String, String>,
    }
    let resp = client
        .get("http://httpbin.org/cookies")
        .send()
        .await
        .unwrap();
    let json = resp.into_json::<CookieResponse>().await.unwrap();
    assert_eq!(json.cookies["key"], "value");

    // test server delete cookie
    _ = client
        .get("http://httpbin.org/cookies/delete?key")
        .send()
        .await
        .unwrap();
    let resp = client
        .get("http://httpbin.org/cookies")
        .send()
        .await
        .unwrap();
    let json = resp.into_json::<CookieResponse>().await.unwrap();
    assert_eq!(json.cookies.len(), 0);
}
