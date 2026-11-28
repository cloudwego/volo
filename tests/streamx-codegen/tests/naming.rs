//! Standard generated bindings, registration-before-incoming, and strict paths.
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use pilota::{pb::Message as _, thrift::Message as _};
use streamx_codegen::{
    names_override::wire_override as renamed,
    names_pb::{alpha, beta},
    names_thrift::{conflicts as c, shadow},
};
#[path = "support/naming.rs"]
mod support;
use support::*;
use tokio::io::{AsyncRead, AsyncWrite};
use volo::net::{Address, incoming::DefaultIncoming};

#[tokio::test]
async fn real_generated_short_names_and_fallback_conflicts_fail_before_incoming() {
    let audit = Audit::default();
    let listened = Arc::new(AtomicUsize::new(0));
    for proto in [false, true] {
        let server = volo_streamx::Server::new()
            .add_service(alpha::echo_streamx::bind(Alpha(audit.clone())));
        let server = if proto {
            server.add_service(beta::echo_streamx::bind(Beta))
        } else {
            server.add_service(shadow::echo_streamx::bind(Shadow))
        };
        let error = server
            .run(MustNotListen(listened.clone()))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("duplicate service short name Echo") && error.contains("alpha.Echo"),
            "{error}"
        );
        assert!(
            error.contains(if proto { "beta.Echo" } else { "shadow.Echo" }),
            "{error}"
        );
    }
    let error = base(&audit)
        .run(MustNotListen(listened.clone()))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("conflicts across services"), "{error}");
    let error = base(&audit)
        .add_fallback_service(c::partial_fallback_streamx::bind(handler(
            "partial", &audit,
        )))
        .run(MustNotListen(listened.clone()))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("SharedB"), "{error}");
    let error = base(&audit)
        .add_fallback_service(c::full_fallback_streamx::bind(handler("full", &audit)))
        .add_fallback_service(c::partial_fallback_streamx::bind(handler(
            "partial", &audit,
        )))
        .run(MustNotListen(listened.clone()))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("only one fallback"), "{error}");
    assert!(
        base(&audit)
            .refuse_traffic_without_service_name(true)
            .prepare()
            .is_ok()
    );
    assert!(
        base(&audit)
            .add_fallback_service(c::full_fallback_streamx::bind(handler("full", &audit)))
            .prepare()
            .is_ok()
    );
    assert_eq!(listened.load(Ordering::Relaxed), 0);
    assert!(audit.0.lock().unwrap().is_empty());
}

#[test]
fn same_logical_binding_is_registered_once_and_wire_overrides_keep_raw_name() {
    let audit = Audit::default();
    let mut registry = volo_streamx::Registry::new();
    let binding = alpha::echo_streamx::bind(Alpha(audit.clone()));
    registry.register(binding.clone()).unwrap();
    let error = registry.register(binding).unwrap_err().to_string();
    assert!(error.contains("duplicate service short name Echo"));
    let registry = registry.freeze().unwrap();
    assert_eq!(
        registry
            .find_thrift("Echo", "First")
            .unwrap()
            .service()
            .wire_name,
        "alpha.Echo"
    );
    let renamed = renamed::renamed_streamx::bind(Renamed(audit));
    assert_eq!(
        (renamed.descriptor().name, renamed.descriptor().wire_name),
        ("Renamed", "custom.Endpoint")
    );
    let mut registry = volo_streamx::Registry::new();
    registry.register(renamed).unwrap();
    let registry = registry.freeze().unwrap();
    for path in [
        "/custom.Endpoint/Call",
        "/Renamed/Call",
        "/other.Renamed/Call",
    ] {
        assert_eq!(
            volo_grpc::server::RouteLookup::find(&*registry, path)
                .unwrap()
                .name(),
            "custom.Endpoint"
        );
    }
    for path in [
        "/Endpoint/Call",
        "/other.Endpoint/Call",
        "/custom.Endpoint/Missing",
        "/Wrong/Call",
    ] {
        assert!(volo_grpc::server::RouteLookup::find(&*registry, path).is_err());
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
async fn connect(address: &Address) -> Box<dyn Io> {
    match address {
        Address::Ip(address) => Box::new(tokio::net::TcpStream::connect(address).await.unwrap()),
        Address::Unix(address) => Box::new(
            tokio::net::UnixStream::connect(address.as_pathname().unwrap())
                .await
                .unwrap(),
        ),
        #[cfg(feature = "shmipc")]
        Address::Shmipc(_) => unreachable!("runtime matrix is TCP/UDS"),
    }
}
fn frame(payload: Bytes) -> Bytes {
    let mut bytes = BytesMut::new();
    bytes.put_u8(0);
    bytes.put_u32(payload.len().try_into().unwrap());
    bytes.extend_from_slice(&payload);
    bytes.freeze()
}
async fn h2_call(
    client: &mut h2::client::SendRequest<Bytes>,
    path: &str,
    thrift: bool,
    payload: Bytes,
    expected: &str,
) -> Bytes {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("http://fixture{path}"))
        .header(
            "content-type",
            if thrift {
                "application/grpc+thrift"
            } else {
                "application/grpc"
            },
        )
        .body(())
        .unwrap();
    let (response, mut upload) = client.send_request(request, false).unwrap();
    upload.send_data(frame(payload), true).unwrap();
    let response = response.await.unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);
    let mut headers = response.headers().clone();
    let mut body = response.into_body();
    let mut bytes = BytesMut::new();
    while let Some(data) = body.data().await {
        let data = data.unwrap();
        body.flow_control().release_capacity(data.len()).unwrap();
        bytes.extend_from_slice(&data);
    }
    if let Some(trailers) = body.trailers().await.unwrap() {
        headers.extend(trailers);
    }
    assert_eq!(headers["grpc-status"], expected, "{path}: {headers:?}");
    if expected == "0" {
        assert_eq!(bytes.get_u8(), 0);
        assert_eq!(bytes.get_u32() as usize, bytes.len());
    }
    bytes.freeze()
}
fn encode<M: pilota::thrift::Message>(value: &M) -> Bytes {
    let mut bytes = pilota::LinkedBytes::new();
    value
        .encode(&mut pilota::thrift::binary::TBinaryProtocol::new(
            &mut bytes, true,
        ))
        .unwrap();
    bytes.into_bytes_mut().freeze()
}

#[tokio::test]
async fn generated_paths_are_strict_without_mutating_thrift_selection_on_tcp_and_uds() {
    tokio::time::timeout(Duration::from_secs(15), async {
        for uds in [false, true] {
            let audit = Audit::default();
            let prepared = base(&audit)
                .add_fallback_service(c::full_fallback_streamx::bind(handler("full", &audit)))
                .add_service(alpha::echo_streamx::bind(Alpha(audit.clone())))
                .add_service(renamed::renamed_streamx::bind(Renamed(audit.clone())))
                .graceful_shutdown_timeout(Duration::from_secs(1))
                .prepare()
                .unwrap();
            let directory = prepared.directory().clone();
            let fallback = directory.find_thrift("", "SharedA").unwrap();
            assert_eq!(fallback.service().name, "FullFallback");
            assert_eq!(
                directory
                    .find_thrift("Wrong", "OnlyA")
                    .unwrap()
                    .service()
                    .name,
                "FirstService"
            );
            assert!(directory.find_thrift("FirstService", "OnlyB").is_none());
            let socket = tempfile::tempdir().unwrap();
            let (address, incoming) = if uds {
                let path = socket.path().join("names.sock");
                (
                    Address::Unix(std::os::unix::net::SocketAddr::from_pathname(&path).unwrap()),
                    DefaultIncoming::from(tokio::net::UnixListener::bind(path).unwrap()),
                )
            } else {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                (
                    Address::Ip(listener.local_addr().unwrap()),
                    DefaultIncoming::from(listener),
                )
            };
            let handle = prepared.handle();
            let server = tokio::spawn(prepared.run(incoming));
            let (mut h2, driver) = h2::client::handshake(connect(&address).await)
                .await
                .unwrap();
            let driver = tokio::spawn(driver);
            let mut value = pilota::LinkedBytes::new();
            alpha::Value {
                text: "request".into(),
            }
            .encode(&mut value)
            .unwrap();
            let value = value.into_bytes_mut().freeze();
            for path in ["/alpha.Echo/First"] {
                let reply = h2_call(&mut h2, path, false, value.clone(), "0").await;
                assert_eq!(alpha::Value::decode(reply).unwrap().text, "alpha");
                assert!(Arc::ptr_eq(
                    &fallback,
                    &directory.find_thrift("", "SharedA").unwrap()
                ));
                assert!(directory.find_thrift("Wrong", "SharedA").is_none());
            }
            for path in [
                "/alpha.Echo/Missing",
                "/Wrong/First",
                "/alpha.Echo/Second",
                "/other.pkg.Echo/First",
                "/Echo/First",
                "/beta.Echo/First",
            ] {
                assert!(
                    h2_call(&mut h2, path, false, value.clone(), "12")
                        .await
                        .is_empty()
                );
            }
            for path in [
                "/custom.Endpoint/Call",
                "/Renamed/Call",
                "/other.Renamed/Call",
            ] {
                let mut reply = h2_call(
                    &mut h2,
                    path,
                    true,
                    encode(&renamed::RenamedCallArgsSend {
                        request: renamed::Value {
                            text: "request".into(),
                        },
                    }),
                    "0",
                )
                .await;
                let mut protocol = pilota::thrift::binary::TBinaryProtocol::new(&mut reply, true);
                assert_eq!(
                    renamed::RenamedCallResultRecv::decode(&mut protocol).unwrap(),
                    renamed::RenamedCallResultRecv::Ok(renamed::Value {
                        text: "renamed".into()
                    })
                );
            }
            for path in [
                "/Endpoint/Call",
                "/other.Endpoint/Call",
                "/custom.Endpoint/Missing",
            ] {
                assert!(
                    h2_call(&mut h2, path, true, Bytes::from_static(&[0]), "12")
                        .await
                        .is_empty()
                );
            }
            let first = c::FirstServiceClientBuilder::new("first")
                .address(address.clone())
                .build();
            assert_eq!(
                first
                    .shared_a(c::Value {
                        text: "request".into()
                    })
                    .await
                    .unwrap()
                    .text,
                "first"
            );
            assert_eq!(
                first
                    .only_a(c::Value {
                        text: "request".into()
                    })
                    .await
                    .unwrap()
                    .text,
                "first"
            );
            assert!(Arc::ptr_eq(
                &fallback,
                &directory.find_thrift("", "SharedA").unwrap()
            ));
            assert_eq!(
                directory
                    .find_thrift("Wrong", "OnlyA")
                    .unwrap()
                    .service()
                    .name,
                "FirstService"
            );
            {
                let calls = audit.0.lock().unwrap();
                assert_eq!(calls.len(), 6);
                assert_eq!(
                    calls
                        .iter()
                        .filter(|(label, service, method)| label == "alpha"
                            && service == "Echo"
                            && method == "First")
                        .count(),
                    1
                );
                assert_eq!(
                    calls
                        .iter()
                        .filter(|(label, _, method)| label == "renamed" && method == "Call")
                        .count(),
                    3
                );
            }
            println!(
                "standard generated routes uds={uds}: 15 RPC, 9 handler calls, same H2 \
                 connection, directory unchanged"
            );
            drop(first);
            drop(h2);
            driver.abort();
            handle.shutdown();
            server.await.unwrap().unwrap();
            let stats = handle.stats();
            assert_eq!(
                (stats.active, stats.active_calls, stats.active_tasks),
                (0, 0, 0)
            );
        }
    })
    .await
    .unwrap();
}
