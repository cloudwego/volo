use std::{
    collections::BTreeMap,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::FutureExt;
use thrift_grpc_codegen::{
    fixture::{Handler, verify_client},
    volo_gen::{grpc_parent::*, grpc_thrift::*},
};
use volo::net::{
    conn::Conn,
    incoming::{DefaultIncoming, Incoming, MakeIncoming},
};
use volo_grpc::{
    Request,
    context::Deadline,
    message::RpcMode,
    server::{NamedService, Server, ServiceBuilder},
    streaming::{StreamContext, StreamHook, StreamStatus},
    thrift::channel,
};

#[derive(Clone, Default)]
struct Events {
    stages: Arc<Mutex<BTreeMap<usize, Vec<&'static str>>>>,
    finished: Arc<Mutex<Vec<volo_grpc::streaming::StreamStatus>>>,
}
impl Events {
    fn push(&self, cx: &volo_grpc::streaming::StreamContext, event: &'static str) {
        self.stages
            .lock()
            .unwrap()
            .entry(cx.id())
            .or_default()
            .push(event);
    }
}
impl volo_grpc::streaming::StreamHook for Events {
    fn on_open(&self, cx: &volo_grpc::streaming::StreamContext) -> Result<(), volo_grpc::Status> {
        self.push(cx, "open");
        Ok(())
    }
    fn on_header(
        &self,
        cx: &volo_grpc::streaming::StreamContext,
        _: &volo_grpc::metadata::MetadataMap,
    ) {
        self.push(cx, "header");
    }
    fn on_send(&self, cx: &volo_grpc::streaming::StreamContext, _: usize) {
        self.push(cx, "send");
    }
    fn on_recv(&self, cx: &volo_grpc::streaming::StreamContext, _: usize) {
        self.push(cx, "recv");
    }
    fn on_trailer(
        &self,
        cx: &volo_grpc::streaming::StreamContext,
        _: &volo_grpc::metadata::MetadataMap,
    ) {
        self.push(cx, "trailer");
    }
    fn on_finish(
        &self,
        cx: &volo_grpc::streaming::StreamContext,
        status: &volo_grpc::streaming::StreamStatus,
    ) {
        self.push(cx, "finish");
        self.finished.lock().unwrap().push(status.clone());
    }
}

#[tokio::test]
async fn generated_mixed_service_uses_one_h2_endpoint() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server_events = Events::default();
        let server_hook = server_events.clone();
        let server = tokio::spawn(async move {
            Server::new()
                .add_service(
                    ServiceBuilder::new(InteropServer::new(Handler))
                        .stream_hook(move |_: &StreamContext| server_hook.clone())
                        .build(),
                )
                .add_service(ServiceBuilder::new(ParentServer::new(Handler)).build())
                .add_service(ServiceBuilder::new(PureUnaryServer::new(Handler)).build())
                .add_service(ServiceBuilder::new(PureStreamingServer::new(Handler)).build())
                .run_with_shutdown(DefaultIncoming::from(listener), async {
                    stopped.await.unwrap();
                    Ok(())
                })
                .await
                .unwrap();
        });
        let client_events = Events::default();
        let client_hook = client_events.clone();
        let client = InteropClientBuilder::new("interop")
            .stream_hook(move |_: &StreamContext| client_hook.clone())
            .address(address)
            .build();
        let passed = verify_client(&client).await.unwrap();
        assert_eq!(passed.len(), 11);
        for (events, received) in [(&client_events, 3), (&server_events, 2)] {
            let finished = events.finished.lock().unwrap();
            assert_eq!(finished.len(), 16);
            assert_eq!(
                finished.iter().filter(|status| status.status.code() == volo_grpc::Code::Internal).count(),
                1,
            );
            assert!(finished.iter().all(|status| matches!(
                status.status.code(),
                volo_grpc::Code::Ok | volo_grpc::Code::Internal,
            )));
            assert_eq!(
                finished.iter().filter(|status| status.declared_exception).count(),
                3,
            );
            assert!(finished.iter().any(|status| status.received_messages == received));
            for stages in events.stages.lock().unwrap().values() {
                assert_eq!(stages.first(), Some(&"open"));
                assert_eq!(stages.last(), Some(&"finish"));
                assert_eq!(stages.iter().filter(|stage| **stage == "finish").count(), 1);
                if stages.len() > 3 {
                    assert!(stages.contains(&"header"));
                    assert!(stages.contains(&"trailer"));
                }
            }
        }
        let unary = PureUnaryClientBuilder::new("interop")
            .address(address)
            .build();
        unary.echo(PureUnaryEchoRequest { request: Empty {} }).await.unwrap();
        let bidi = PureStreamingClientBuilder::new("interop")
            .address(address)
            .build();
        let mut response = bidi.bidi(futures::stream::iter([Empty {}]))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.message().await.unwrap(), Some(Empty {}));
        assert!(response.message().await.unwrap().is_none());
        drop((response, bidi, unary, client));
        stop.send(()).unwrap();
        server.await.unwrap();
        assert_eq!(InteropServer::<Handler>::NAME, "api.Interop");
        assert_eq!(ParentServer::<Handler>::NAME, "parent.Parent");
        assert_eq!(
            <thrift_grpc_codegen::volo_gen::grpc_no_namespace::GlobalServer<NoNamespace> as NamedService>::NAME,
            "grpc_no_namespace.Global",
        );
    })
    .await
    .expect("generated loopback must make progress");
}

#[derive(Debug)]
struct CountedIncoming(DefaultIncoming, Arc<AtomicUsize>);

impl Incoming for CountedIncoming {
    async fn accept(&mut self) -> io::Result<Option<Conn>> {
        let connection = self.0.accept().await?;
        if connection.is_some() {
            self.1.fetch_add(1, Ordering::Relaxed);
        }
        Ok(connection)
    }
}

impl MakeIncoming for CountedIncoming {
    type Incoming = Self;

    async fn make_incoming(self) -> io::Result<Self> {
        Ok(self)
    }
}

#[derive(Clone, Default)]
struct MixedProgress {
    client_read: Arc<tokio::sync::Notify>,
    server_finished: Arc<tokio::sync::Notify>,
}

impl StreamHook for MixedProgress {
    fn on_recv(&self, context: &StreamContext, _: usize) {
        if context.mode() == Some(RpcMode::ClientStreaming) {
            self.client_read.notify_one();
        }
    }

    fn on_finish(&self, context: &StreamContext, _: &StreamStatus) {
        if context.mode() == Some(RpcMode::ServerStreaming) {
            self.server_finished.notify_one();
        }
    }
}

#[tokio::test]
async fn unary_progresses_on_the_same_connection_as_three_active_streams() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accepts = Arc::new(AtomicUsize::new(0));
        let incoming = CountedIncoming(DefaultIncoming::from(listener), accepts.clone());
        let progress = MixedProgress::default();
        let hooks = progress.clone();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            Server::new()
                .add_service(
                    ServiceBuilder::new(InteropServer::new(Handler))
                        .stream_hook(move |_: &StreamContext| hooks.clone())
                        .build(),
                )
                .run_with_shutdown(incoming, async {
                    stopped.await.unwrap();
                    Ok(())
                })
                .await
                .unwrap();
        });
        let client = InteropClientBuilder::new("mixed")
            .address(address)
            .http2_init_stream_window_size(1024u32)
            .build();

        let (mut upload_tx, upload) = channel(2);
        let uploader = client.clone();
        let upload_call = tokio::spawn(async move { uploader.client(upload).await });
        let uploaded = Message {
            payload: Some("upload".into()),
            sequence: None,
        };
        upload_tx.send(&uploaded).await.unwrap();
        progress.client_read.notified().await;

        let (mut bidi_tx, bidi_input) = channel(2);
        let echoed = Message {
            payload: Some("echo".into()),
            sequence: Some(1),
        };
        bidi_tx.send(&echoed).await.unwrap();
        let mut bidi = client.bidi(bidi_input).await.unwrap().into_inner();
        assert_eq!(bidi.message().await.unwrap(), Some(echoed));

        // Leave the next reply blocked by the receive window.
        let payload = pilota::Bytes::from(vec![b's'; 16 * 1024]);
        let mut request = Request::new(Message {
            payload: Some(payload.clone()),
            sequence: None,
        });
        request.extensions_mut().insert(Deadline::new(
            tokio::time::Instant::now() + Duration::from_secs(5),
        ));
        let mut slow = client.server(request).await.unwrap().into_inner();
        assert_eq!(
            slow.message().await.unwrap().unwrap().payload,
            Some(payload)
        );
        assert!(slow.stream_context().unwrap().deadline().is_some());
        assert!(bidi.stream_context().unwrap().deadline().is_none());

        let unary = Message {
            payload: Some("unary".into()),
            sequence: Some(2),
        };
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            client.unary(InteropUnaryRequest {
                request: unary.clone(),
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.into_inner(), unary);
        assert!(!upload_call.is_finished());
        assert!(progress.server_finished.notified().now_or_never().is_none());
        drop(slow);
        progress.server_finished.notified().await;

        let echoed = Message {
            payload: Some("after-cancel".into()),
            sequence: Some(3),
        };
        bidi_tx.send(&echoed).await.unwrap();
        assert_eq!(bidi.message().await.unwrap(), Some(echoed));
        drop(bidi_tx);
        assert!(bidi.message().await.unwrap().is_none());
        drop(upload_tx);
        let reply = upload_call.await.unwrap().unwrap().into_inner();
        assert_eq!(reply.payload, uploaded.payload);
        assert_eq!(reply.sequence, Some(1));
        assert_eq!(accepts.load(Ordering::Relaxed), 1);
        drop((bidi, client));
        stop.send(()).unwrap();
        server.await.unwrap();
    })
    .await
    .expect("multiplexed calls must complete within the test deadline");
}

#[test]
#[should_panic(expected = "[VOLO] Invalid route")]
fn duplicate_generated_service_routes_are_rejected() {
    Server::new()
        .add_service(ServiceBuilder::new(InteropServer::new(Handler)).build())
        .add_service(ServiceBuilder::new(InteropServer::new(Handler)).build());
}

struct NoNamespace;

impl thrift_grpc_codegen::volo_gen::grpc_no_namespace::Global for NoNamespace {
    async fn echo(
        &self,
        request: Request<thrift_grpc_codegen::volo_gen::grpc_no_namespace::GlobalEchoRequest>,
    ) -> Result<
        volo_grpc::Response<thrift_grpc_codegen::volo_gen::grpc_no_namespace::Empty>,
        volo_grpc::Status,
    > {
        Ok(volo_grpc::Response::new(request.into_inner().request))
    }
}

#[test]
fn dual_transport_modules_share_messages_and_exception_enum_identity() {
    use thrift_grpc_codegen::{shared as grpc, ttstream as thrift};
    let message = thrift::grpc_parent::Message {
        payload: Some("same type".into()),
        sequence: None,
    };
    let message: grpc::grpc_parent::Message = message;
    let request = grpc::grpc_thrift::InteropUnaryRequest { request: message };
    let _: thrift::grpc_thrift::InteropUnaryArgsSend = request;
    let exception = thrift::grpc_thrift::InteropThrowException::Other(thrift::grpc_parent::Other {
        code: Some(7),
    });
    let _: grpc::grpc_thrift::InteropThrowException = exception;
    let tree = std::sync::Arc::new(thrift::shared_wrappers::Node {
        next: Some(Box::new(thrift::shared_wrappers::Node { next: None })),
    });
    let tree: std::sync::Arc<grpc::shared_wrappers::Node> = tree;
    let request = grpc::shared_wrappers::WrappedEchoRequest { req: tree };
    let _: thrift::shared_wrappers::WrappedEchoArgsSend = request;
}
