#![cfg(feature = "thrift")]

use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use motore::Service;
use pilota::thrift::{ApplicationException, ApplicationExceptionKind};
use tokio::sync::{mpsc, watch};
use volo::{
    net::conn::PeekedConn,
    streaming::{Metadata, RequestStream, ResponseSender, RpcError, RpcResult, StreamContext},
};
use volo_grpc::{
    Code, RecvEntryMessage, Request, Response, Status,
    body::BoxBody,
    codec::{
        compression::CompressionEncoding,
        decode::Kind,
        encode::tests::EchoRequest,
        thrift::{ThriftDecoder, ThriftRecvStream},
    },
    context::ServerContext,
    message::{
        CodecOptions, MethodDescriptor, PayloadKind, PayloadShape, RpcMode, ServiceDescriptor,
    },
    server::{MetaService, NamedService, Router, ServiceBuilder, serve_detected},
    streaming::{
        StreamContext as NativeContext,
        response::{self, ChannelConfig, ProtobufStream},
    },
    tracing::DefaultProvider,
};

struct ProtoInput(volo_grpc::RecvStream<EchoRequest>);
impl RecvEntryMessage for ProtoInput {
    fn from_body(
        method: Option<&str>,
        body: BoxBody,
        kind: Kind,
        compression: Option<CompressionEncoding>,
    ) -> Result<Self, Status> {
        Self::from_body_with_options(
            method,
            body,
            kind,
            compression,
            CodecOptions::new(Default::default()),
        )
    }
    fn from_body_with_options(
        _: Option<&str>,
        body: BoxBody,
        kind: Kind,
        compression: Option<CompressionEncoding>,
        options: CodecOptions,
    ) -> Result<Self, Status> {
        Ok(Self(
            volo_grpc::RecvStream::new(body, kind, compression)
                .with_limits(options.limits)
                .with_context(options.context),
        ))
    }
}
struct ThriftInput(ThriftRecvStream<ApplicationException>);
impl RecvEntryMessage for ThriftInput {
    fn payload_kind() -> PayloadKind {
        PayloadKind::Thrift
    }
    fn from_body(
        method: Option<&str>,
        body: BoxBody,
        kind: Kind,
        compression: Option<CompressionEncoding>,
    ) -> Result<Self, Status> {
        Self::from_body_with_options(
            method,
            body,
            kind,
            compression,
            CodecOptions::new(Default::default()),
        )
    }
    fn from_body_with_options(
        _: Option<&str>,
        body: BoxBody,
        kind: Kind,
        compression: Option<CompressionEncoding>,
        options: CodecOptions,
    ) -> Result<Self, Status> {
        Ok(Self(
            ThriftRecvStream::with_decoder(body, kind, compression, ThriftDecoder::default())
                .with_limits(options.limits)
                .with_context(options.context),
        ))
    }
}
#[derive(Clone)]
struct Proto {
    seen: mpsc::UnboundedSender<NativeContext>,
    reclaimed: Arc<AtomicUsize>,
}
#[derive(Clone)]
struct Thrift {
    seen: mpsc::UnboundedSender<NativeContext>,
    reclaimed: Arc<AtomicUsize>,
}
impl NamedService for Proto {
    const NAME: &'static str = "public.Proto";
    fn descriptor() -> Option<ServiceDescriptor> {
        Some(ServiceDescriptor {
            payload: PayloadKind::Protobuf,
            methods: &[
                MethodDescriptor {
                    name: "Call",
                    mode: RpcMode::Bidirectional,
                    payload: PayloadShape::ProtobufMessage,
                },
                MethodDescriptor {
                    name: "Collect",
                    mode: RpcMode::ClientStreaming,
                    payload: PayloadShape::ProtobufMessage,
                },
            ],
        })
    }
}
impl NamedService for Thrift {
    const NAME: &'static str = "public.Thrift";
    fn descriptor() -> Option<ServiceDescriptor> {
        Some(ServiceDescriptor {
            payload: PayloadKind::Thrift,
            methods: &[
                MethodDescriptor {
                    name: "Call",
                    mode: RpcMode::Bidirectional,
                    payload: PayloadShape::StreamingRawStruct,
                },
                MethodDescriptor {
                    name: "Collect",
                    mode: RpcMode::ClientStreaming,
                    payload: PayloadShape::StreamingRawStruct,
                },
            ],
        })
    }
}
fn config() -> ChannelConfig {
    ChannelConfig {
        capacity: 1,
        max_buffered_bytes: 128 * 1024,
        max_message_size: 64 * 1024,
        unsafe_codec: false,
    }
}
impl Service<ServerContext, Request<ProtoInput>> for Proto {
    type Response = Response<ProtobufStream<EchoRequest>>;
    type Error = Status;
    async fn call(
        &self,
        _: &mut ServerContext,
        request: Request<ProtoInput>,
    ) -> Result<Self::Response, Status> {
        let native = request.extensions().get::<NativeContext>().unwrap().clone();
        self.seen.send(native.clone()).unwrap();
        let input = response::request(&native, request.into_inner().0);
        let reclaimed = self.reclaimed.clone();
        if native
            .shared_context()
            .metadata()
            .get("x-action")
            .unwrap()
            .starts_with(b"single")
        {
            return response::protobuf_unary(native, move |cx| {
                single_handler(cx, input, reclaimed)
            })
            .await;
        }
        response::protobuf(native, config(), move |cx, output| {
            handler(cx, input, output, reclaimed)
        })
        .await
    }
}
impl Service<ServerContext, Request<ThriftInput>> for Thrift {
    type Response = Response<volo_grpc::thrift::SendStream<ApplicationException>>;
    type Error = Status;
    async fn call(
        &self,
        _: &mut ServerContext,
        request: Request<ThriftInput>,
    ) -> Result<Self::Response, Status> {
        let native = request.extensions().get::<NativeContext>().unwrap().clone();
        self.seen.send(native.clone()).unwrap();
        let input = response::request(&native, request.into_inner().0);
        let reclaimed = self.reclaimed.clone();
        if native
            .shared_context()
            .metadata()
            .get("x-action")
            .unwrap()
            .starts_with(b"single")
        {
            return response::thrift_unary(native, move |cx| single_handler(cx, input, reclaimed))
                .await;
        }
        response::thrift(native, config(), move |cx, output| {
            handler(cx, input, output, reclaimed)
        })
        .await
    }
}
struct Owned(Arc<AtomicUsize>);
impl Drop for Owned {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Release);
    }
}
async fn handler<T: Send + 'static>(
    cx: StreamContext,
    input: RequestStream<T>,
    mut output: ResponseSender<T>,
    reclaimed: Arc<AtomicUsize>,
) -> RpcResult<()> {
    let _handler_owned = Owned(reclaimed.clone());
    let action = String::from_utf8(cx.metadata().get("x-action").unwrap().to_vec()).unwrap();
    let message = receive(&cx, input, reclaimed).await?;
    if action == "empty" {
        return Ok(());
    }
    for _ in 0..if action == "flood" { 4096 } else { 1 } {
        output.send(&message).await?;
    }
    if action == "error" {
        return Err(business_error());
    }
    Ok(())
}
async fn single_handler<T: Send + 'static>(
    cx: StreamContext,
    input: RequestStream<T>,
    reclaimed: Arc<AtomicUsize>,
) -> RpcResult<T> {
    let _handler_owned = Owned(reclaimed.clone());
    let action = String::from_utf8(cx.metadata().get("x-action").unwrap().to_vec()).unwrap();
    let message = receive(&cx, input, reclaimed).await?;
    match action.as_str() {
        "single-error" => Err(business_error()),
        "single-panic" => panic!("handler panic"),
        "single-wait" => futures::future::pending().await,
        _ => Ok(message),
    }
}
fn business_error() -> RpcError {
    RpcError::Biz {
        code: 17,
        message: "business failed".into(),
        extra: Default::default(),
    }
}
async fn receive<T: Send + 'static>(
    cx: &StreamContext,
    mut input: RequestStream<T>,
    reclaimed: Arc<AtomicUsize>,
) -> RpcResult<T> {
    assert_eq!(StreamContext::current().unwrap().id(), cx.id());
    let message = input.recv().await?.unwrap();
    assert!(input.recv().await?.is_none());
    drop(input);
    let owned = Owned(reclaimed);
    let (started, ready) = tokio::sync::oneshot::channel();
    let work = cx.spawn_scoped(async move {
        let _owned = owned;
        let _ = started.send(());
        futures::future::pending::<()>().await;
    });
    drop(work);
    ready.await.unwrap();
    let mut header = Metadata::default();
    header.append("x-header", "one");
    header.append("x-header", "two");
    cx.response_headers(header)?;
    let mut trailer = Metadata::default();
    trailer.insert("x-trailer", "final");
    cx.response_trailers(trailer)?;
    Ok(message)
}

async fn start(
    window: u32,
) -> (
    h2::client::SendRequest<Bytes>,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
    watch::Sender<()>,
    mpsc::UnboundedReceiver<NativeContext>,
    Arc<AtomicUsize>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (seen, contexts) = mpsc::unbounded_channel();
    let reclaimed = Arc::new(AtomicUsize::new(0));
    let proto = ServiceBuilder::new(Proto {
        seen: seen.clone(),
        reclaimed: reclaimed.clone(),
    })
    .build::<ProtoInput, ProtobufStream<EchoRequest>>();
    let thrift = ServiceBuilder::new(Thrift {
        seen,
        reclaimed: reclaimed.clone(),
    })
    .build::<ThriftInput, volo_grpc::thrift::SendStream<ApplicationException>>();
    let router = Router::new().add_service(proto).add_service(thrift);
    let service = MetaService::new(router, DefaultProvider);
    let (shutdown, stop) = watch::channel(());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let io = PeekedConn::peek8(socket.into()).await.unwrap().unwrap();
        serve_detected(io, service, Default::default(), stop)
            .await
            .unwrap();
    });
    let socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let (client, connection) = h2::client::Builder::new()
        .initial_window_size(window)
        .handshake(socket)
        .await
        .unwrap();
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    (client, driver, server, shutdown, contexts, reclaimed)
}
async fn call(
    client: &mut h2::client::SendRequest<Bytes>,
    thrift: bool,
    action: &str,
    text: &str,
) -> (h2::client::ResponseFuture, h2::SendStream<Bytes>) {
    call_with_timeout(client, thrift, action, text, None).await
}
async fn call_with_timeout(
    client: &mut h2::client::SendRequest<Bytes>,
    thrift: bool,
    action: &str,
    text: &str,
    timeout: Option<&str>,
) -> (h2::client::ResponseFuture, h2::SendStream<Bytes>) {
    futures::future::poll_fn(|task| client.poll_ready(task))
        .await
        .unwrap();
    let service = if thrift {
        "public.Thrift"
    } else {
        "public.Proto"
    };
    let method = if action.starts_with("single") {
        "Collect"
    } else {
        "Call"
    };
    let path = format!("/{service}/{method}");
    let mut request = http::Request::builder()
        .method("POST")
        .uri(format!("http://localhost{path}"))
        .header(
            "content-type",
            if thrift {
                "application/grpc+thrift"
            } else {
                "application/grpc"
            },
        )
        .header("x-action", action);
    if let Some(timeout) = timeout {
        request = request.header("grpc-timeout", timeout);
    }
    let request = request.body(()).unwrap();
    let (response, mut upload) = client.send_request(request, false).unwrap();
    let payload = if thrift {
        volo_thrift_bytes(text)
    } else {
        use pilota::pb::Message;
        let mut data = pilota::LinkedBytes::new();
        EchoRequest {
            message: text.to_owned().into(),
        }
        .encode(&mut data)
        .unwrap();
        data.into_bytes_mut().freeze()
    };
    let mut frame = vec![0];
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    upload.send_data(Bytes::from(frame), true).unwrap();
    (response, upload)
}
fn volo_thrift_bytes(text: &str) -> Bytes {
    use pilota::thrift::Message;
    let mut data = pilota::LinkedBytes::new();
    ApplicationException::new(ApplicationExceptionKind::UNKNOWN, text.to_owned())
        .encode(&mut pilota::thrift::binary::TBinaryProtocol::new(
            &mut data, true,
        ))
        .unwrap();
    data.into_bytes_mut().freeze()
}

#[tokio::test]
async fn single_response_handlers_return_one_message_or_error_without_a_success_payload() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (mut client, driver, server, shutdown, mut contexts, reclaimed) = start(65535).await;
        for thrift in [false, true] {
            for action in ["single", "single-error", "single-panic"] {
                let (response, _upload) = call(&mut client, thrift, action, "reply").await;
                let native = contexts.recv().await.unwrap();
                let response = response.await.unwrap();
                assert_eq!(response.headers().get_all("x-header").iter().count(), 2);
                let mut body = response.into_body();
                let mut bytes = Vec::new();
                while let Some(data) = body.data().await {
                    let data = data.unwrap();
                    bytes.extend_from_slice(&data);
                    body.flow_control().release_capacity(data.len()).unwrap();
                }
                let trailers = body.trailers().await.unwrap().unwrap();
                assert_eq!(
                    trailers["grpc-status"],
                    if action == "single" { "0" } else { "13" }
                );
                assert_eq!(trailers["x-trailer"], "final");
                if action == "single" {
                    let payload = if thrift {
                        volo_thrift_bytes("reply")
                    } else {
                        Bytes::from_static(b"\x0a\x05reply")
                    };
                    assert_eq!(&bytes[5..], payload);
                    assert_eq!(
                        u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize,
                        payload.len()
                    );
                } else {
                    assert!(
                        bytes.is_empty(),
                        "an error must not synthesize a success message"
                    );
                    if action == "single-error" {
                        assert_eq!(trailers["biz-status"], "17");
                    }
                }
                native.children_reclaimed().await;
                assert_eq!(
                    native.status().unwrap().status.code(),
                    if action == "single" {
                        Code::Ok
                    } else {
                        Code::Internal
                    }
                );
            }
        }
        assert_eq!(reclaimed.load(Ordering::Acquire), 12);
        drop(client);
        driver.abort();
        let _ = driver.await;
        let _ = shutdown.send(());
        server.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn both_payloads_drain_committed_data_before_business_status_and_keep_neighbors_usable() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (mut client, driver, server, shutdown, mut contexts, reclaimed) = start(65535).await;
        for thrift in [false, true] {
            for action in ["error", "ok"] {
                let (response, _upload) = call(&mut client, thrift, action, "hello").await;
                let native = contexts.recv().await.unwrap();
                let response = response.await.unwrap();
                assert_eq!(response.headers().get_all("x-header").iter().count(), 2);
                let mut body = response.into_body();
                let mut bytes = Vec::new();
                while let Some(data) = body.data().await {
                    let data = data.unwrap();
                    bytes.extend_from_slice(&data);
                    body.flow_control().release_capacity(data.len()).unwrap();
                }
                let trailers = body.trailers().await.unwrap().unwrap();
                assert_eq!(
                    trailers["grpc-status"],
                    if action == "error" { "13" } else { "0" }
                );
                assert_eq!(trailers["x-trailer"], "final");
                assert!(
                    bytes.len() > 5,
                    "candidate error must follow the committed DATA"
                );
                if action == "error" {
                    assert_eq!(trailers["biz-status"], "17");
                }
                while native.status().is_none() {
                    tokio::task::yield_now().await;
                }
                assert_eq!(
                    native.status().unwrap().status.code(),
                    if action == "error" {
                        Code::Internal
                    } else {
                        Code::Ok
                    }
                );
                native.children_reclaimed().await;
            }
        }
        assert_eq!(reclaimed.load(Ordering::Acquire), 8);
        drop(client);
        driver.abort();
        let _ = driver.await;
        let _ = shutdown.send(());
        server.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn deadline_reclaims_suspended_handler_and_zero_window_producer_without_body_progress() {
    tokio::time::timeout(Duration::from_secs(5), async {
        for thrift in [false, true] {
            for action in ["single-wait", "flood"] {
                let (mut client, driver, server, shutdown, mut contexts, reclaimed) =
                    start(0).await;
                let (response, upload) = call_with_timeout(
                    &mut client,
                    thrift,
                    action,
                    &"x".repeat(24 * 1024),
                    Some("100m"),
                )
                .await;
                let native = contexts.recv().await.unwrap();
                // Do not poll the response until the server has timed out. A
                // pending handler or blocked DATA must not delay cancellation.
                assert_eq!(native.cancelled().await.code(), Code::DeadlineExceeded);
                native.children_reclaimed().await;
                while reclaimed.load(Ordering::Acquire) < 2 {
                    tokio::task::yield_now().await;
                }
                assert_eq!(
                    native.status().unwrap().status.code(),
                    Code::DeadlineExceeded
                );
                let response = response.await.unwrap();
                let status = if let Some(status) = response.headers().get("grpc-status") {
                    status.clone()
                } else {
                    let mut body = response.into_body();
                    assert!(body.data().await.is_none());
                    body.trailers().await.unwrap().unwrap()["grpc-status"].clone()
                };
                assert_eq!(status, "4");
                drop(upload);
                let (probe, _) = call(&mut client, !thrift, "empty", "neighbor").await;
                let neighbor = contexts.recv().await.unwrap();
                let mut body = probe.await.unwrap().into_body();
                assert!(body.data().await.is_none());
                assert_eq!(body.trailers().await.unwrap().unwrap()["grpc-status"], "0");
                assert_eq!(neighbor.status().unwrap().status.code(), Code::Ok);
                drop(client);
                driver.abort();
                let _ = driver.await;
                let _ = shutdown.send(());
                server.await.unwrap();
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn zero_window_peer_reset_reclaims_production_without_waiting_for_body_progress() {
    tokio::time::timeout(Duration::from_secs(5), async {
        for thrift in [false, true] {
            let (mut client, driver, server, shutdown, mut contexts, reclaimed) = start(0).await;
            let (response, mut upload) =
                call(&mut client, thrift, "flood", &"x".repeat(24 * 1024)).await;
            let native = contexts.recv().await.unwrap();
            let response = response.await.unwrap();
            assert!(native.status().is_none());
            upload.send_reset(h2::Reason::CANCEL);
            let status = native.cancelled().await;
            assert_eq!(status.code(), Code::Cancelled);
            assert_eq!(
                status
                    .source()
                    .and_then(|source| source.downcast_ref::<h2::Error>())
                    .and_then(h2::Error::reason),
                Some(h2::Reason::CANCEL)
            );
            native.children_reclaimed().await;
            while reclaimed.load(Ordering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
            drop(response);
            let (probe, _) = call(&mut client, !thrift, "empty", "probe").await;
            let neighbor = contexts.recv().await.unwrap();
            let mut body = probe.await.unwrap().into_body();
            assert!(body.data().await.is_none());
            assert_eq!(body.trailers().await.unwrap().unwrap()["grpc-status"], "0");
            assert_eq!(neighbor.status().unwrap().status.code(), Code::Ok);
            drop(client);
            driver.abort();
            let _ = driver.await;
            let _ = shutdown.send(());
            server.await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn an_uncommitted_business_result_loses_to_reset_and_committed_success_survives_late_no_error()
 {
    tokio::time::timeout(Duration::from_secs(5), async {
        for thrift in [false, true] {
            let (mut client, driver, server, shutdown, mut contexts, _) = start(0).await;
            let (response, mut upload) = call(&mut client, thrift, "error", "queued").await;
            let native = contexts.recv().await.unwrap();
            let response = response.await.unwrap();
            native.children_reclaimed().await;
            assert!(
                native.status().is_none(),
                "business error is still only a candidate under a zero window"
            );
            upload.send_reset(h2::Reason::CANCEL);
            assert_eq!(native.cancelled().await.code(), Code::Cancelled);
            assert!(
                native.trailers().is_none(),
                "no business Trailer was committed"
            );
            drop(response);
            let (probe, mut upload) = call(&mut client, thrift, "empty", "probe").await;
            let native = contexts.recv().await.unwrap();
            let mut body = probe.await.unwrap().into_body();
            assert!(body.data().await.is_none());
            assert_eq!(body.trailers().await.unwrap().unwrap()["grpc-status"], "0");
            upload.send_reset(h2::Reason::NO_ERROR);
            tokio::task::yield_now().await;
            assert_eq!(native.status().unwrap().status.code(), Code::Ok);
            drop(client);
            driver.abort();
            let _ = driver.await;
            let _ = shutdown.send(());
            server.await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn connection_loss_records_transport_failure_before_body_drop_and_reclaims_the_handler() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (mut client, driver, server, shutdown, mut contexts, reclaimed) = start(0).await;
        let (response, upload) = call(&mut client, false, "flood", &"x".repeat(24 * 1024)).await;
        let native = contexts.recv().await.unwrap();
        let response = response.await.unwrap();
        driver.abort();
        let _ = driver.await;
        let status = native.cancelled().await;
        assert_eq!(
            status.code(),
            Code::Unavailable,
            "connection loss must not be attributed to local Body Drop: {status:?}"
        );
        assert!(matches!(
            native.shared_context().cancelled().await,
            RpcError::Transport {
                phase: volo::streaming::IoPhase::Read | volo::streaming::IoPhase::Write,
                committed: true,
                ..
            }
        ));
        native.children_reclaimed().await;
        assert_eq!(reclaimed.load(Ordering::Acquire), 2);
        drop(response);
        drop(upload);
        drop(client);
        let _ = shutdown.send(());
        server.await.unwrap();
    })
    .await
    .unwrap();
}
