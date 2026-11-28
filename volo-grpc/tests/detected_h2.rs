//! Real H2 requests through the detected-connection entry point and the legacy server.
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use futures::{StreamExt, stream};
use http::{HeaderMap, StatusCode};
use motore::Service;
use tower::ServiceExt;
use volo::net::{conn::PeekedConn, incoming::DefaultIncoming};
use volo_grpc::{
    BoxStream, RecvEntryMessage, RecvStream, Request, Response, SendEntryMessage, Status,
    body::BoxBody,
    codec::{
        compression::CompressionEncoding,
        decode::Kind,
        encode::{encode, tests::EchoRequest},
    },
    context::ServerContext,
    message::{MethodDescriptor, PayloadKind, PayloadShape, RpcMode, ServiceDescriptor},
    server::{MetaService, NamedService, Router, Server, ServiceBuilder, serve_detected},
    tracing::DefaultProvider,
};

static DECODES: AtomicUsize = AtomicUsize::new(0);

struct Input(RecvStream<EchoRequest>);
impl RecvEntryMessage for Input {
    fn from_body(
        _: Option<&str>,
        body: BoxBody,
        kind: Kind,
        compression: Option<CompressionEncoding>,
    ) -> Result<Self, Status> {
        DECODES.fetch_add(1, Ordering::Relaxed);
        Ok(Self(RecvStream::new(body, kind, compression)))
    }
}

struct Output(EchoRequest);
impl SendEntryMessage for Output {
    fn into_body(
        self,
        compression: Option<CompressionEncoding>,
    ) -> BoxStream<'static, Result<http_body::Frame<Bytes>, Status>> {
        encode(stream::iter([Ok(self.0)]), compression)
    }
}

#[derive(Clone)]
struct Echo {
    calls: Arc<AtomicUsize>,
    managed: bool,
}
impl NamedService for Echo {
    const NAME: &'static str = "public.Echo";
    fn descriptor() -> Option<ServiceDescriptor> {
        Some(ServiceDescriptor {
            payload: PayloadKind::Protobuf,
            methods: &[MethodDescriptor {
                name: "Call",
                mode: RpcMode::Unary,
                payload: PayloadShape::ProtobufMessage,
            }],
        })
    }
}
impl Service<ServerContext, Request<Input>> for Echo {
    type Response = Response<Output>;
    type Error = Status;
    async fn call(
        &self,
        cx: &mut ServerContext,
        request: Request<Input>,
    ) -> Result<Self::Response, Status> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        assert_eq!(
            request
                .extensions()
                .get::<volo_grpc::streaming::StreamContext>()
                .is_some(),
            self.managed
        );
        assert!(cx.rpc_info.caller().address().is_some());
        if let Some(native) = request
            .extensions()
            .get::<volo_grpc::streaming::StreamContext>()
        {
            let shared = native.shared_context();
            assert_eq!(shared.metadata().get("x-request").unwrap(), "application");
            let mut headers = volo::streaming::Metadata::default();
            headers.append("x-shared-header", "first");
            headers.append("x-shared-header", "second");
            shared.response_headers(headers)?;
            let mut trailers = volo::streaming::Metadata::default();
            trailers.insert("x-shared-trailer", "final");
            shared.response_trailers(trailers)?;
        }
        let value = request.into_inner().0.next().await.unwrap()?;
        Ok(Response::new(Output(value)))
    }
}

async fn call(
    client: &mut h2::client::SendRequest<Bytes>,
    address: std::net::SocketAddr,
    path: &str,
    types: &[&str],
) -> (StatusCode, HeaderMap, HeaderMap) {
    let mut request = http::Request::builder()
        .method("POST")
        .uri(format!("http://{address}{path}"))
        .body(())
        .unwrap();
    for content_type in types {
        request
            .headers_mut()
            .append("content-type", content_type.parse().unwrap());
    }
    request.headers_mut().insert(
        volo_grpc::metadata::SOURCE_SERVICE,
        "public-client".parse().unwrap(),
    );
    request
        .headers_mut()
        .insert("x-request", "application".parse().unwrap());
    let (response, mut send) = client.send_request(request, false).unwrap();
    // An empty, valid Protobuf message with its gRPC frame prefix.
    send.send_data(Bytes::from_static(b"\0\0\0\0\0"), true)
        .unwrap();
    let response = response.await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let mut body = response.into_body();
    while let Some(bytes) = body.data().await {
        let bytes = bytes.unwrap();
        body.flow_control().release_capacity(bytes.len()).unwrap();
    }
    let trailers = body.trailers().await.unwrap().unwrap_or_default();
    (status, headers, trailers)
}

fn code<'a>(headers: &'a HeaderMap, trailers: &'a HeaderMap) -> &'a http::HeaderValue {
    headers
        .get("grpc-status")
        .or_else(|| trailers.get("grpc-status"))
        .unwrap()
}

#[tokio::test]
async fn detected_h2_checks_proto_before_decode_and_legacy_acceptance_is_preserved() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for strict in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let service = ServiceBuilder::new(Echo {
                calls: calls.clone(),
                managed: strict,
            })
            .build::<Input, Output>();
            let (shutdown, mut stop) = tokio::sync::watch::channel(());
            let server = tokio::spawn(async move {
                if strict {
                    let router = Router::new().add_service(service);
                    let service =
                        MetaService::new(router, DefaultProvider).map_future(|future| async move {
                            let response = future.await;
                            // Make native completion observable before the HTTP response is
                            // returned.
                            tokio::task::yield_now().await;
                            response
                        });
                    let (socket, _) = listener.accept().await.unwrap();
                    let io = PeekedConn::peek8(socket.into()).await.unwrap().unwrap();
                    serve_detected(io, service, Default::default(), stop)
                        .await
                        .unwrap();
                } else {
                    Server::new()
                        .add_service(service)
                        .run_with_shutdown(DefaultIncoming::from(listener), async move {
                            let _ = stop.changed().await;
                            Ok(())
                        })
                        .await
                        .unwrap();
                }
            });
            let (mut client, connection) =
                h2::client::handshake(tokio::net::TcpStream::connect(address).await.unwrap())
                    .await
                    .unwrap();
            let driver = tokio::spawn(connection);
            for types in [
                &[][..],
                &["text/plain"][..],
                &["application/grpc", "application/grpc"][..],
            ] {
                let before = DECODES.load(Ordering::Relaxed);
                let (http, headers, trailers) =
                    call(&mut client, address, "/public.Echo/Call", types).await;
                if strict {
                    assert_eq!(http, StatusCode::UNSUPPORTED_MEDIA_TYPE);
                    assert_eq!(DECODES.load(Ordering::Relaxed), before);
                } else {
                    assert_eq!(http, StatusCode::OK);
                    assert_eq!(code(&headers, &trailers), "0");
                    assert_eq!(DECODES.load(Ordering::Relaxed), before + 1);
                }
            }
            let before = DECODES.load(Ordering::Relaxed);
            let (http, headers, trailers) = call(
                &mut client,
                address,
                "/public.Echo/Missing",
                &["text/plain"],
            )
            .await;
            assert_eq!(http, StatusCode::OK);
            assert_eq!(code(&headers, &trailers), "12");
            assert_eq!(DECODES.load(Ordering::Relaxed), before);
            if strict {
                let (http, headers, trailers) = call(
                    &mut client,
                    address,
                    "/public.Echo/Call",
                    &["application/grpc+thrift"],
                )
                .await;
                assert_eq!(http, StatusCode::OK);
                assert_eq!(code(&headers, &trailers), "12");
                assert_eq!(DECODES.load(Ordering::Relaxed), before);
                assert_eq!(calls.load(Ordering::Relaxed), 0);
            }
            for content_type in [
                "application/grpc",
                "application/grpc+proto",
                "application/grpc+json",
            ] {
                let (http, headers, trailers) =
                    call(&mut client, address, "/public.Echo/Call", &[content_type]).await;
                if strict && content_type == "application/grpc+json" {
                    assert_eq!(http, StatusCode::UNSUPPORTED_MEDIA_TYPE);
                    continue;
                }
                assert_eq!(http, StatusCode::OK);
                assert_eq!(code(&headers, &trailers), "0");
                if strict {
                    assert_eq!(headers.get_all("x-shared-header").iter().count(), 2);
                    assert_eq!(trailers["x-shared-trailer"], "final");
                }
            }
            assert_eq!(calls.load(Ordering::Relaxed), if strict { 2 } else { 6 });
            drop(client);
            driver.abort();
            let _ = driver.await;
            shutdown.send(()).unwrap();
            server.await.unwrap();
        }
    })
    .await
    .expect("H2 policy matrix exceeded its timeout");
}
