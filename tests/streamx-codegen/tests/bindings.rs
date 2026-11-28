use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::StreamExt;
use streamx_codegen::{protobuf::streamx::presence as pb, thrift::chat::*};
use volo::net::incoming::DefaultIncoming;
use volo_streamx::{RequestStream, ResponseSender, RpcResult, StreamContext};

#[path = "support/cardinality.rs"]
mod cardinality;

struct Chat {
    calls: AtomicUsize,
    fire: AtomicUsize,
    barriers: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
}
impl Chat {
    async fn wait(&self, cx: &StreamContext, request: &Message) {
        check_framework_scope(cx);
        if request.text == "block" {
            let (started, release) = self.barriers.as_ref().unwrap();
            started.notify_one();
            release.notified().await;
            assert!(
                cx.cancellation().is_none(),
                "ping-pong work must survive peer EOF"
            );
        }
    }
    fn echo(&self, mut value: Message) -> Message {
        self.calls.fetch_add(1, Ordering::Relaxed);
        value.text = format!("{}@shared", value.text).into();
        value
    }
}
impl chat_service_streamx::Handler for Chat {
    async fn get(
        &self,
        cx: StreamContext,
        request: Message,
    ) -> RpcResult<Result<Message, ChatServiceGetException>> {
        self.wait(&cx, &request).await;
        assert_eq!(cx.service(), "ChatService");
        assert_eq!(cx.payload(), Some(volo_streamx::PayloadKind::Thrift));
        assert_eq!(
            cx.route(),
            Some(volo_streamx::RpcRoute {
                service: "ChatService",
                method: "Get",
                payload: volo_streamx::PayloadKind::Thrift,
                mode: Some(volo_streamx::RpcMode::Unary),
            })
        );
        assert_eq!(StreamContext::current().unwrap().id(), cx.id());
        if request.text == "path-proof" {
            let path = cx.with_extensions(|extensions| {
                extensions
                    .get::<volo_grpc::server::OriginalGrpcPath>()
                    .unwrap()
                    .0
                    .clone()
            });
            assert_eq!(path, "/other.package.ChatService/Get");
        }
        if request.sequence == -1 {
            return Ok(Err(ChatServiceGetException::Missing(Missing {
                reason: "missing".into(),
            })));
        }
        if request.sequence == -2 {
            return Ok(Err(ChatServiceGetException::Denied(Denied { code: 7 })));
        }
        Ok(Ok(self.echo(request)))
    }
    async fn ping(&self, _: StreamContext, _: ()) -> RpcResult<()> {
        Ok(())
    }
    async fn add(&self, _: StreamContext, request: ChatServiceAddArgsRecv) -> RpcResult<i64> {
        Ok(i64::from(request.left) + request.right)
    }
    async fn upload(
        &self,
        _: StreamContext,
        mut input: RequestStream<Message>,
    ) -> RpcResult<Message> {
        let mut count = 0;
        while input.recv().await?.is_some() {
            count += 1;
        }
        Ok(self.echo(Message {
            text: "upload".into(),
            sequence: count,
        }))
    }
    async fn watch(
        &self,
        _: StreamContext,
        request: Message,
        mut output: ResponseSender<Message>,
    ) -> RpcResult<()> {
        for sequence in 0..3 {
            output
                .send(&self.echo(Message {
                    text: request.text.clone(),
                    sequence,
                }))
                .await?;
        }
        Ok(())
    }
    async fn talk(
        &self,
        _: StreamContext,
        mut input: RequestStream<Message>,
        mut output: ResponseSender<Message>,
    ) -> RpcResult<()> {
        while let Some(request) = input.recv().await? {
            output.send(&self.echo(request)).await?;
        }
        Ok(())
    }
    async fn fire(&self, cx: StreamContext, request: Message) -> RpcResult<()> {
        self.wait(&cx, &request).await;
        self.fire.fetch_add(1, Ordering::Release);
        if request.sequence == -1 {
            return Err(volo_streamx::RpcError::Biz {
                code: 17,
                message: "oneway failed".into(),
                extra: Default::default(),
            });
        }
        assert_ne!(request.sequence, -2, "oneway panic");
        Ok(())
    }
}
struct Presence;
impl pb::presence_service_streamx::Handler for Presence {
    async fn get(&self, cx: StreamContext, req: pb::Event) -> RpcResult<pb::Event> {
        assert_eq!(StreamContext::current().unwrap().id(), cx.id());
        Ok(req)
    }
    async fn upload(
        &self,
        _: StreamContext,
        mut input: RequestStream<pb::Event>,
    ) -> RpcResult<pb::Event> {
        let mut reply = pb::Event::default();
        while let Some(value) = input.recv().await? {
            reply.sequence += value.sequence;
        }
        Ok(reply)
    }
    async fn watch(
        &self,
        _: StreamContext,
        req: pb::Event,
        mut output: ResponseSender<pb::Event>,
    ) -> RpcResult<()> {
        output.send(&req).await
    }
    async fn talk(
        &self,
        _: StreamContext,
        mut input: RequestStream<pb::Event>,
        mut output: ResponseSender<pb::Event>,
    ) -> RpcResult<()> {
        while let Some(value) = input.recv().await? {
            output.send(&value).await?;
        }
        Ok(())
    }
}
fn message(sequence: i32) -> Message {
    Message {
        text: "hello".into(),
        sequence,
    }
}

#[tokio::test]
async fn generated_thrift_response_preserves_an_application_exception() {
    use pilota::thrift::{
        ApplicationException, ApplicationExceptionKind, Message as _, TMessageIdentifier,
        TMessageType, ThriftException,
    };
    use volo_thrift::EntryMessage;
    let expected =
        ApplicationException::new(ApplicationExceptionKind::UNKNOWN_METHOD, "unknown call");
    let mut encoded = pilota::LinkedBytes::new();
    expected
        .encode(&mut pilota::thrift::binary::TBinaryProtocol::new(
            &mut encoded,
            true,
        ))
        .unwrap();
    let original = encoded.into_bytes_mut().freeze();
    let mut bytes = original.clone();
    let identifier = TMessageIdentifier::new("Get".into(), TMessageType::Exception, 17);
    let result = chat_service_streamx::ThriftResponse::decode(
        &mut pilota::thrift::binary::TBinaryProtocol::new(&mut bytes, true),
        &identifier,
    );
    assert!(
        matches!(result,Err(ThriftException::Application(error)) if error.kind()==expected.kind() && error.message()==expected.message())
    );
    let mut protocol = pilota::thrift::binary::TAsyncBinaryProtocol::new(
        tokio::io::BufReader::new(original.as_ref()),
    );
    let result =
        chat_service_streamx::ThriftResponse::decode_async(&mut protocol, &identifier).await;
    assert!(
        matches!(result,Err(ThriftException::Application(error)) if error.kind()==expected.kind() && error.message()==expected.message())
    );
}

#[tokio::test]
async fn one_handler_serves_native_thrift_ttstream_and_grpc_without_the_old_traits() {
    tokio::time::timeout(Duration::from_secs(10), async {
        use volo_thrift::server::NamedService;
        let shared = Arc::new(Chat {
            calls: AtomicUsize::new(0),
            fire: AtomicUsize::new(0),
            barriers: None,
        });
        let bound = chat_service_streamx::BoundService::from_arc(shared.clone());
        assert!(Arc::ptr_eq(bound.handler(), &shared));
        assert!(Arc::ptr_eq(bound.clone().handler(), &shared));
        assert_eq!(bound.descriptor().methods.len(), 7);
        assert_eq!(
            bound
                .descriptor()
                .methods
                .iter()
                .filter(|method| method.oneway)
                .count(),
            1
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let thrift_address = listener.local_addr().unwrap();
        let thrift = bound.thrift();
        let streaming = thrift.streaming_router().unwrap();
        let thrift_server = tokio::spawn(async move {
            volo_thrift::server::Server::new(thrift)
                .streaming_router(streaming)
                .run(DefaultIncoming::from(listener))
                .await
                .unwrap();
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let grpc_address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let grpc_server = tokio::spawn(async move {
            volo_grpc::server::Server::new()
                .add_service(volo_grpc::server::ServiceBuilder::new(bound.grpc()).build())
                .add_service(
                    volo_grpc::server::ServiceBuilder::new(
                        pb::presence_service_streamx::bind(Presence).grpc(),
                    )
                    .build(),
                )
                .run_with_shutdown(DefaultIncoming::from(listener), async {
                    stopped.await.unwrap();
                    Ok(())
                })
                .await
                .unwrap();
        });
        let thrift = ChatServiceClientBuilder::new("chat")
            .address(thrift_address)
            .build();
        let grpc = streamx_codegen::grpc::chat::ChatServiceClientBuilder::new("chat")
            .address(grpc_address)
            .build();
        let unary = thrift.get(message(7)).await.unwrap();
        assert!(
            matches!(unary,volo_thrift::MaybeException::Ok(value) if value.text=="hello@shared")
        );
        assert_eq!(thrift.add(3, 4).await.unwrap(), 7);
        for seq in [-1, -2] {
            assert!(matches!(
                thrift.get(message(seq)).await.unwrap(),
                volo_thrift::MaybeException::Exception(_)
            ));
            assert!(
                grpc.get(streamx_codegen::grpc::chat::ChatServiceGetRequest {
                    request: message(seq)
                })
                .await
                .unwrap()
                .into_inner()
                .is_err()
            );
        }
        assert_eq!(
            grpc.get(streamx_codegen::grpc::chat::ChatServiceGetRequest {
                request: message(7)
            })
            .await
            .unwrap()
            .into_inner()
            .unwrap()
            .text,
            "hello@shared"
        );
        assert_eq!(
            grpc.add(streamx_codegen::grpc::chat::ChatServiceAddRequest { left: 3, right: 4 })
                .await
                .unwrap()
                .into_inner(),
            7
        );
        for seq in [0, -1, -2] {
            thrift.fire(message(seq)).await.unwrap();
            thrift.ping().await.unwrap();
        }
        let mut upload = thrift.upload().await.unwrap();
        upload.send(&message(0)).await.unwrap();
        upload.send(&message(1)).await.unwrap();
        assert_eq!(upload.close_and_recv().await.unwrap().sequence, 2);
        assert_eq!(
            grpc.upload(futures::stream::iter([message(0), message(1)]))
                .await
                .unwrap()
                .into_inner()
                .sequence,
            2
        );
        let mut watch = thrift.watch(message(0)).await.unwrap();
        for seq in 0..3 {
            assert_eq!(watch.recv().await.unwrap().unwrap().sequence, seq);
        }
        assert!(watch.recv().await.unwrap().is_none());
        let mut watch = grpc.watch(message(0)).await.unwrap().into_inner();
        for seq in 0..3 {
            assert_eq!(watch.message().await.unwrap().unwrap().sequence, seq);
        }
        assert!(watch.message().await.unwrap().is_none());
        let (mut talk_send, mut talk_recv) = thrift.talk().await.unwrap().into_split();
        talk_send.send(&message(9)).await.unwrap();
        talk_send.close_send().await.unwrap();
        assert_eq!(
            talk_recv.recv().await.unwrap().unwrap().text,
            "hello@shared"
        );
        assert!(talk_recv.recv().await.unwrap().is_none());
        let mut talk = grpc
            .talk(futures::stream::iter([message(9)]))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(talk.message().await.unwrap().unwrap().text, "hello@shared");
        assert!(talk.message().await.unwrap().is_none());
        let presence = pb::PresenceServiceClientBuilder::new("presence")
            .address(grpc_address)
            .build();
        let event = pb::Event {
            text: "protobuf".into(),
            sequence: 7,
        };
        assert_eq!(
            presence.get(event.clone()).await.unwrap().into_inner(),
            event
        );
        assert_eq!(
            presence
                .upload(futures::stream::iter([event.clone(), event.clone()]))
                .await
                .unwrap()
                .into_inner()
                .sequence,
            14
        );
        let mut watch = Box::pin(presence.watch(event.clone()).await.unwrap().into_inner());
        assert_eq!(watch.next().await.transpose().unwrap(), Some(event.clone()));
        assert!(watch.next().await.transpose().unwrap().is_none());
        let mut talk = Box::pin(
            presence
                .talk(futures::stream::iter([event.clone()]))
                .await
                .unwrap()
                .into_inner(),
        );
        assert_eq!(talk.next().await.transpose().unwrap(), Some(event));
        assert!(talk.next().await.transpose().unwrap().is_none());
        assert_eq!(shared.calls.load(Ordering::Relaxed), 12);
        assert_eq!(shared.fire.load(Ordering::Acquire), 3);
        drop((watch, talk));
        drop((grpc, thrift, presence));
        stop.send(()).unwrap();
        grpc_server.await.unwrap();
        thrift_server.abort();
        let _ = thrift_server.await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn accepted_pingpong_unary_and_oneway_finish_after_the_socket_is_closed() {
    use pilota::thrift::{Message as _, TOutputProtocol};
    use tokio::io::AsyncWriteExt;
    tokio::time::timeout(Duration::from_secs(5), async {
        for detected in [false, true] {
            let started = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let shared = Arc::new(Chat {
                calls: AtomicUsize::new(0),
                fire: AtomicUsize::new(0),
                barriers: Some((started.clone(), release.clone())),
            });
            let bound = chat_service_streamx::BoundService::from_arc(shared.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let builder = volo_thrift::server::Server::new(bound.thrift());
                if detected {
                    let prepared = builder.prepare().unwrap();
                    let mut connections = tokio::task::JoinSet::new();
                    loop {
                        let (socket, _) = listener.accept().await.unwrap();
                        let driver = prepared.connection();
                        connections.spawn(async move {
                            let io = volo::net::conn::PeekedConn::peek8(socket.into())
                                .await
                                .unwrap()
                                .unwrap();
                            driver
                                .serve_detected(
                                    io.into_buffered_split(8192),
                                    volo_thrift::server::DetectedThrift::PingPong,
                                )
                                .await
                                .unwrap();
                        });
                    }
                } else {
                    builder.run(DefaultIncoming::from(listener)).await.unwrap();
                }
            });
            for oneway in [false, true] {
                let mut payload = pilota::LinkedBytes::new();
                let mut protocol = pilota::thrift::binary::TBinaryProtocol::new(&mut payload, true);
                protocol
                    .write_message_begin(&pilota::thrift::TMessageIdentifier::new(
                        if oneway { "Fire" } else { "Get" }.into(),
                        if oneway {
                            pilota::thrift::TMessageType::OneWay
                        } else {
                            pilota::thrift::TMessageType::Call
                        },
                        71,
                    ))
                    .unwrap();
                let request = Message {
                    text: "block".into(),
                    sequence: 0,
                };
                if oneway {
                    ChatServiceFireArgsSend { request }
                        .encode(&mut protocol)
                        .unwrap();
                } else {
                    ChatServiceGetArgsSend { request }
                        .encode(&mut protocol)
                        .unwrap();
                }
                protocol.write_message_end().unwrap();
                let payload = payload.into_bytes_mut();
                let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
                socket
                    .write_all(&(payload.len() as u32).to_be_bytes())
                    .await
                    .unwrap();
                socket.write_all(&payload).await.unwrap();
                started.notified().await;
                socket.shutdown().await.unwrap();
                drop(socket);
                release.notify_one();
                while if oneway {
                    shared.fire.load(Ordering::Acquire)
                } else {
                    shared.calls.load(Ordering::Acquire)
                } == 0
                {
                    tokio::task::yield_now().await;
                }
            }
            assert_eq!(shared.calls.load(Ordering::Acquire), 1);
            assert_eq!(shared.fire.load(Ordering::Acquire), 1);
            server.abort();
            let _ = server.await;
        }
    })
    .await
    .unwrap();
}

type NativeCodec = volo_thrift::codec::DefaultMakeCodec<
    volo_thrift::codec::default::ttheader::MakeTTHeaderCodec<
        volo_thrift::codec::default::framed::MakeFramedCodec<
            volo_thrift::codec::default::thrift::MakeThriftCodec,
        >,
    >,
>;
type NativeReader = volo::net::conn::OwnedReadHalf;
type NativeWriter = volo::net::conn::OwnedWriteHalf;

#[derive(Clone, Default)]
struct CountCodec {
    native: NativeCodec,
    probes: Arc<AtomicUsize>,
    unbuffered: Arc<AtomicUsize>,
    buffered: Arc<AtomicUsize>,
    reject_buffered: bool,
}

struct CountDecoder<D> {
    inner: D,
    probes: Arc<AtomicUsize>,
}

impl<D: volo_thrift::codec::Decoder> volo_thrift::codec::Decoder for CountDecoder<D> {
    async fn probe_ttstream(&mut self) -> std::io::Result<bool> {
        self.probes.fetch_add(1, Ordering::Relaxed);
        self.inner.probe_ttstream().await
    }

    fn into_stream_reader(self) -> Result<Box<dyn volo_thrift::codec::StreamRead>, Self> {
        let Self { inner, probes } = self;
        inner
            .into_stream_reader()
            .map_err(|inner| Self { inner, probes })
    }

    async fn decode<
        Msg: Send + volo_thrift::EntryMessage,
        Cx: volo_thrift::context::ThriftContext,
    >(
        &mut self,
        cx: &mut Cx,
    ) -> Result<Option<volo_thrift::ThriftMessage<Msg>>, pilota::thrift::ThriftException> {
        self.inner.decode(cx).await
    }

    #[cfg(feature = "shmipc")]
    fn shmipc_helper(&self) -> volo::net::shmipc::ShmipcHelper {
        self.inner.shmipc_helper()
    }
}

impl volo_thrift::codec::MakeCodec<NativeReader, NativeWriter> for CountCodec {
    type Encoder =
        <NativeCodec as volo_thrift::codec::MakeCodec<NativeReader, NativeWriter>>::Encoder;
    type Decoder = CountDecoder<
        <NativeCodec as volo_thrift::codec::MakeCodec<NativeReader, NativeWriter>>::Decoder,
    >;

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_buffered(&self) -> bool {
        !self.reject_buffered
    }

    fn make_codec(
        &self,
        reader: NativeReader,
        writer: NativeWriter,
    ) -> (Self::Encoder, Self::Decoder) {
        self.unbuffered.fetch_add(1, Ordering::Relaxed);
        let (encoder, inner) = self.native.make_codec(reader, writer);
        (
            encoder,
            CountDecoder {
                inner,
                probes: self.probes.clone(),
            },
        )
    }

    fn make_codec_from_buffered(
        &self,
        reader: volo::util::buf_reader::BufReader<NativeReader>,
        writer: NativeWriter,
    ) -> std::io::Result<(Self::Encoder, Self::Decoder)> {
        self.buffered.fetch_add(1, Ordering::Relaxed);
        let (encoder, inner) = self.native.make_codec_from_buffered(reader, writer)?;
        Ok((
            encoder,
            CountDecoder {
                inner,
                probes: self.probes.clone(),
            },
        ))
    }
}

struct CountLayer(Arc<AtomicUsize>);
struct CountService<S> {
    inner: S,
    calls: Arc<AtomicUsize>,
}
impl<S> volo::Layer<S> for CountLayer {
    type Service = CountService<S>;

    fn layer(self, inner: S) -> Self::Service {
        CountService {
            inner,
            calls: self.0,
        }
    }
}
impl<S, R: Send + 'static> volo::Service<volo_thrift::context::ServerContext, R> for CountService<S>
where
    S: volo::Service<volo_thrift::context::ServerContext, R> + Send + Sync,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut volo_thrift::context::ServerContext,
        request: R,
    ) -> Result<Self::Response, Self::Error> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.call(cx, request).await
    }
}

#[tokio::test]
async fn prepared_drivers_keep_layers_and_only_probe_in_the_explicit_entry() {
    use volo_thrift::server::{DetectedThrift, NamedService as _};
    tokio::time::timeout(Duration::from_secs(10), async {
        for detected in [false, true] {
            let codec = CountCodec::default();
            let calls = Arc::new(AtomicUsize::new(0));
            let hooks = Arc::new(AtomicUsize::new(0));
            let hook_calls = hooks.clone();
            let bound = chat_service_streamx::bind(Chat {
                calls: AtomicUsize::new(0),
                fire: AtomicUsize::new(0),
                barriers: None,
            });
            let thrift = bound.thrift();
            let router = if detected {
                let mut registry = volo_streamx::Registry::new();
                registry.register(bound).unwrap();
                registry.freeze().unwrap().ttstream_router()
            } else {
                thrift.streaming_router().unwrap()
            };
            let builder = volo_thrift::server::Server::new(thrift)
                .streaming_router(router)
                .make_codec(codec.clone())
                .layer(CountLayer(calls.clone()))
                .register_shutdown_hook(move || {
                    Box::pin(async move {
                        hook_calls.fetch_add(1, Ordering::Relaxed);
                    })
                });
            #[cfg(feature = "multiplex")]
            let builder = builder.multiplex(true);
            let mut prepared = builder.prepare().unwrap();
            prepared.connection().validate_buffered_handoff().unwrap();
            let control = prepared.control();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (stop, stopped) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                tokio::pin!(stopped);
                let mut connections = tokio::task::JoinSet::new();
                loop {
                    let socket = tokio::select! {
                        _ = &mut stopped => break,
                        socket = listener.accept() => socket.unwrap().0,
                    };
                    let driver = prepared.connection();
                    connections.spawn(async move {
                        if detected {
                            let io = volo::net::conn::PeekedConn::peek8(socket.into())
                                .await
                                .unwrap()
                                .unwrap();
                            let protocol =
                                if volo_thrift::transport::ttstream::frame::matches_prefix(
                                    io.prefix(),
                                ) {
                                    DetectedThrift::Ttstream
                                } else {
                                    DetectedThrift::PingPong
                                };
                            driver
                                .serve_detected(io.into_buffered_split(8192), protocol)
                                .await
                                .unwrap();
                        } else {
                            driver.serve(socket.into()).await;
                        }
                    });
                }
                prepared.run_shutdown_hooks().await;
                prepared.run_shutdown_hooks().await;
                control.begin_drain();
                control.close_idle();
                while let Some(result) = connections.join_next().await {
                    result.unwrap();
                }
                assert_eq!(control.active_connections(), 0);
            });
            let client = ChatServiceClientBuilder::new("chat")
                .address(address)
                .build();
            assert!(matches!(
                client.get(message(0)).await.unwrap(),
                volo_thrift::MaybeException::Ok(_)
            ));
            let mut upload = client.upload().await.unwrap();
            upload.send(&message(1)).await.unwrap();
            assert_eq!(upload.close_and_recv().await.unwrap().sequence, 1);
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            drop(client);
            stop.send(()).unwrap();
            server.await.unwrap();
            assert_eq!(hooks.load(Ordering::Relaxed), 1);
            if detected {
                assert_eq!(codec.unbuffered.load(Ordering::Relaxed), 0);
                assert_eq!(codec.buffered.load(Ordering::Relaxed), 1);
                assert_eq!(codec.probes.load(Ordering::Relaxed), 0);
            } else {
                assert!(codec.unbuffered.load(Ordering::Relaxed) >= 2);
                assert_eq!(codec.buffered.load(Ordering::Relaxed), 0);
                assert_eq!(
                    codec.probes.load(Ordering::Relaxed),
                    codec.unbuffered.load(Ordering::Relaxed)
                );
            }
        }
    })
    .await
    .unwrap();
}

#[test]
fn unsupported_buffered_codec_is_rejected_without_a_listener() {
    let bound = chat_service_streamx::bind(Chat {
        calls: AtomicUsize::new(0),
        fire: AtomicUsize::new(0),
        barriers: None,
    });
    let prepared = volo_thrift::server::Server::new(bound.thrift())
        .make_codec(CountCodec {
            reject_buffered: true,
            ..Default::default()
        })
        .prepare()
        .unwrap();
    assert_eq!(
        prepared
            .connection()
            .validate_buffered_handoff()
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Unsupported
    );
}

fn grpc_frame(payload: bytes::Bytes) -> bytes::Bytes {
    let mut frame = Vec::with_capacity(payload.len() + 5);
    frame.push(0);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    frame.into()
}

fn thrift_bytes(value: &impl pilota::thrift::Message) -> bytes::Bytes {
    let mut bytes = pilota::LinkedBytes::new();
    value
        .encode(&mut pilota::thrift::binary::TBinaryProtocol::new(
            &mut bytes, true,
        ))
        .unwrap();
    bytes.into_bytes_mut().freeze()
}

fn raw_request(
    client: &mut h2::client::SendRequest<bytes::Bytes>,
    address: std::net::SocketAddr,
    path: &str,
    types: &[&str],
    payload: bytes::Bytes,
    end: bool,
) -> (h2::client::ResponseFuture, h2::SendStream<bytes::Bytes>) {
    let mut request = http::Request::builder()
        .method("POST")
        .uri(format!("http://{address}{path}"))
        .body(())
        .unwrap();
    request.headers_mut().insert(
        volo_grpc::metadata::SOURCE_SERVICE,
        "fixture".parse().unwrap(),
    );
    for value in types {
        request
            .headers_mut()
            .append("content-type", value.parse().unwrap());
    }
    let (response, mut send) = client.send_request(request, false).unwrap();
    send.send_data(grpc_frame(payload), end).unwrap();
    (response, send)
}

async fn raw_response(
    response: h2::client::ResponseFuture,
) -> (http::StatusCode, http::HeaderMap, bytes::Bytes) {
    let response = response.await.unwrap();
    let http_status = response.status();
    let mut headers = response.headers().clone();
    let mut body = response.into_body();
    let mut payload = Vec::new();
    while let Some(bytes) = body.data().await {
        let bytes = bytes.unwrap();
        payload.extend_from_slice(&bytes);
        body.flow_control().release_capacity(bytes.len()).unwrap();
    }
    if let Some(trailers) = body.trailers().await.unwrap() {
        headers.extend(trailers);
    }
    (http_status, headers, payload.into())
}

#[tokio::test]
async fn shared_directory_routes_both_payloads_on_one_prepared_h2_connection() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let shared = Arc::new(Chat {
            calls: AtomicUsize::new(0),
            fire: AtomicUsize::new(0),
            barriers: None,
        });
        let bound = chat_service_streamx::BoundService::from_arc(shared.clone());
        let mut registry = volo_streamx::Registry::new();
        registry.register(bound).unwrap();
        registry
            .register(pb::presence_service_streamx::bind(Presence))
            .unwrap();
        assert!(
            registry.freeze().is_err(),
            "duplicate methods need an explicit routing policy"
        );
        let mut registry = volo_streamx::Registry::new();
        registry
            .register(chat_service_streamx::BoundService::from_arc(shared.clone()))
            .unwrap();
        registry
            .register(pb::presence_service_streamx::bind(Presence))
            .unwrap();
        registry.fallback("ChatService");
        let registry = registry.freeze().unwrap();
        let selected = registry.find_thrift("", "Get").unwrap();
        assert_eq!(selected.service().name, "ChatService");
        assert!(registry.find_thrift("wrong", "Get").is_none());
        let prepared = volo_grpc::server::Server::with_router(registry.grpc_router()).prepare();
        prepared.validate_streamx().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let server_accepts = accepted.clone();
        let (stop, stopped) = tokio::sync::watch::channel(());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            server_accepts.fetch_add(1, Ordering::Relaxed);
            let io = volo::net::conn::PeekedConn::peek8(socket.into())
                .await
                .unwrap()
                .unwrap();
            prepared.serve_detected(io, stopped).await.unwrap();
        });
        let (mut client, connection) =
            h2::client::handshake(tokio::net::TcpStream::connect(address).await.unwrap())
                .await
                .unwrap();
        let driver = tokio::spawn(connection);

        // Keep a Thrift bidi request open while serving Protobuf and Thrift unary RPCs.
        let (talk, mut upload) = raw_request(
            &mut client,
            address,
            "/ChatService/Talk",
            &["application/grpc+thrift"],
            thrift_bytes(&message(4)),
            false,
        );
        let talk = talk.await.unwrap();
        assert_eq!(talk.headers()["content-type"], "application/grpc+thrift");
        let mut talk = talk.into_body();
        let first = talk.data().await.unwrap().unwrap();
        talk.flow_control().release_capacity(first.len()).unwrap();
        let event = pb::Event {
            text: "protobuf".into(),
            sequence: 9,
        };
        let payload: bytes::Bytes =
            pilota::pb::Message::encode_to_vec(&event, &mut Default::default()).into();
        let (proto_talk, mut proto_upload) = raw_request(
            &mut client,
            address,
            "/streamx.presence.PresenceService/Talk",
            &["application/grpc"],
            payload.clone(),
            false,
        );
        assert_ne!(upload.stream_id(), proto_upload.stream_id());
        let proto_talk = proto_talk.await.unwrap();
        assert_eq!(proto_talk.headers()["content-type"], "application/grpc");
        let mut proto_talk = proto_talk.into_body();
        let first = proto_talk.data().await.unwrap().unwrap();
        proto_talk
            .flow_control()
            .release_capacity(first.len())
            .unwrap();
        let (response, _) = raw_request(
            &mut client,
            address,
            "/streamx.presence.PresenceService/Get",
            &["application/grpc"],
            payload.clone(),
            true,
        );
        let (status, headers, body) = raw_response(response).await;
        assert_eq!(status, http::StatusCode::OK);
        assert_eq!(headers["grpc-status"], "0");
        assert_eq!(body, grpc_frame(payload.clone()));
        let request = ChatServiceGetArgsSend {
            request: Message {
                text: "path-proof".into(),
                sequence: 7,
            },
        };
        let (response, _) = raw_request(
            &mut client,
            address,
            "/other.package.ChatService/Get",
            &["application/grpc+thrift"],
            thrift_bytes(&request),
            true,
        );
        let (_, headers, body) = raw_response(response).await;
        assert_eq!(headers["grpc-status"], "0");
        assert!(!body.is_empty());
        assert_eq!(shared.calls.load(Ordering::Relaxed), 2);

        for types in [
            &[][..],
            &["text/plain"][..],
            &["application/grpc", "application/grpc"][..],
        ] {
            let (response, _) = raw_request(
                &mut client,
                address,
                "/streamx.presence.PresenceService/Get",
                types,
                payload.clone(),
                true,
            );
            let (status, _, _) = raw_response(response).await;
            assert_eq!(status, http::StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }
        for (path, types) in [
            ("/ChatService/Get", &["application/grpc"][..]),
            (
                "/streamx.presence.PresenceService/Get",
                &["application/grpc+thrift"][..],
            ),
            ("/Wrong/Ping", &[][..]),
            ("/ChatService/Missing", &["text/plain"][..]),
            ("/ChatService/Fire", &["application/grpc+thrift"][..]),
        ] {
            let (response, _) =
                raw_request(&mut client, address, path, types, payload.clone(), true);
            let (_, headers, _) = raw_response(response).await;
            assert_eq!(headers["grpc-status"], "12", "{path}");
        }
        assert_eq!(shared.calls.load(Ordering::Relaxed), 2);
        assert_eq!(shared.fire.load(Ordering::Relaxed), 0);
        upload.send_data(bytes::Bytes::new(), true).unwrap();
        while let Some(bytes) = talk.data().await {
            let bytes = bytes.unwrap();
            talk.flow_control().release_capacity(bytes.len()).unwrap();
        }
        assert_eq!(talk.trailers().await.unwrap().unwrap()["grpc-status"], "0");
        proto_upload.send_data(bytes::Bytes::new(), true).unwrap();
        while let Some(bytes) = proto_talk.data().await {
            let bytes = bytes.unwrap();
            proto_talk
                .flow_control()
                .release_capacity(bytes.len())
                .unwrap();
        }
        assert_eq!(
            proto_talk.trailers().await.unwrap().unwrap()["grpc-status"],
            "0"
        );
        let (response, _) = raw_request(
            &mut client,
            address,
            "/streamx.presence.PresenceService/Get",
            &["application/grpc+proto"],
            payload.clone(),
            true,
        );
        assert_eq!(raw_response(response).await.1["grpc-status"], "0");
        assert_eq!(accepted.load(Ordering::Relaxed), 1);
        assert!(Arc::ptr_eq(
            &selected,
            &registry.find_thrift("", "Get").unwrap()
        ));
        assert!(registry.find_thrift("wrong", "Get").is_none());
        stop.send(()).unwrap();
        drop((client, upload, talk, proto_upload, proto_talk));
        server.await.unwrap();
        driver.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}

#[test]
fn prepared_h2_rejects_http1_before_a_listener_is_created() {
    let prepared = volo_grpc::server::Server::new()
        .accept_http1(true)
        .prepare();
    assert_eq!(
        prepared.validate_streamx().unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
}

struct Archive(Arc<AtomicUsize>);
impl archive_service_streamx::Handler for Archive {
    async fn get(&self, cx: StreamContext, mut req: Message) -> RpcResult<Message> {
        assert_eq!(cx.service(), "ArchiveService");
        self.0.fetch_add(1, Ordering::Relaxed);
        req.text = "archive".into();
        Ok(req)
    }
}

fn chat() -> Arc<Chat> {
    Arc::new(Chat {
        calls: AtomicUsize::new(0),
        fire: AtomicUsize::new(0),
        barriers: None,
    })
}

#[derive(Clone)]
struct GlobalLayer {
    tag: u8,
    entered: Arc<AtomicUsize>,
    exited: Arc<AtomicUsize>,
}
struct GlobalService<S> {
    inner: S,
    layer: GlobalLayer,
}
impl<S> volo::Layer<S> for GlobalLayer {
    type Service = GlobalService<S>;
    fn layer(self, inner: S) -> Self::Service {
        GlobalService { inner, layer: self }
    }
}
#[derive(Default)]
struct LayerTrace(Vec<u8>);
#[derive(Default)]
struct SharedHookCounts {
    created: AtomicUsize,
    opened: AtomicUsize,
    finished: AtomicUsize,
    sent: AtomicUsize,
    received: AtomicUsize,
    headers: AtomicUsize,
    trailers: AtomicUsize,
    half_send: AtomicUsize,
    half_recv: AtomicUsize,
    violations: AtomicUsize,
    records: std::sync::Mutex<Vec<String>>,
}
struct HookIdentity(usize);
tokio::task_local! { static HOOK_SCOPE: usize; }
struct FrameworkScope(usize);
impl volo_streamx::StreamScope for FrameworkScope {
    fn enter(&self, _: &StreamContext, operation: &mut dyn FnMut()) {
        HOOK_SCOPE.sync_scope(self.0, operation);
    }
}
fn check_framework_scope(cx: &StreamContext) {
    if let Some(id) =
        cx.with_extensions(|extensions| extensions.get::<HookIdentity>().map(|id| id.0))
    {
        assert_eq!(HOOK_SCOPE.try_with(|scope| *scope).ok(), Some(id));
    }
}
struct SharedObserver {
    counts: Arc<SharedHookCounts>,
    identity: usize,
    closed: std::sync::atomic::AtomicBool,
    sent: AtomicUsize,
    received: AtomicUsize,
}
impl SharedObserver {
    fn check(&self, cx: &StreamContext) {
        if self.closed.load(Ordering::Acquire)
            || cx.route().is_none_or(|route| {
                route.service != cx.service()
                    || route.method != cx.method()
                    || route.mode != cx.mode()
                    || Some(route.payload) != cx.payload()
                    || route.payload
                        != if cx.service() == "PresenceService" {
                            volo_streamx::PayloadKind::Protobuf
                        } else {
                            volo_streamx::PayloadKind::Thrift
                        }
            })
            || StreamContext::current().is_none_or(|current| current.id() != cx.id())
            || HOOK_SCOPE.try_with(|scope| *scope).ok() != Some(self.identity)
            || cx.with_extensions(|extensions| {
                extensions
                    .get::<HookIdentity>()
                    .is_none_or(|id| id.0 != self.identity)
            })
        {
            self.counts.violations.fetch_add(1, Ordering::Relaxed);
        }
    }
}
impl volo_streamx::StreamHook for SharedObserver {
    fn on_open(&self, cx: &StreamContext) -> RpcResult<()> {
        self.check(cx);
        self.counts.opened.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn on_send(&self, cx: &StreamContext, _: usize) {
        self.check(cx);
        self.sent.fetch_add(1, Ordering::Relaxed);
        self.counts.sent.fetch_add(1, Ordering::Relaxed);
    }
    fn on_recv(&self, cx: &StreamContext, _: usize) {
        self.check(cx);
        self.received.fetch_add(1, Ordering::Relaxed);
        self.counts.received.fetch_add(1, Ordering::Relaxed);
    }
    fn on_header(
        &self,
        cx: &StreamContext,
        _: volo_streamx::StreamDirection,
        _: &volo_streamx::Metadata,
    ) {
        self.check(cx);
        self.counts.headers.fetch_add(1, Ordering::Relaxed);
    }
    fn on_trailer(
        &self,
        cx: &StreamContext,
        _: volo_streamx::StreamDirection,
        _: &volo_streamx::Metadata,
    ) {
        self.check(cx);
        self.counts.trailers.fetch_add(1, Ordering::Relaxed);
    }
    fn on_half_close(&self, cx: &StreamContext, direction: volo_streamx::StreamDirection) {
        self.check(cx);
        match direction {
            volo_streamx::StreamDirection::Send => &self.counts.half_send,
            volo_streamx::StreamDirection::Recv => &self.counts.half_recv,
        }
        .fetch_add(1, Ordering::Relaxed);
    }
    fn on_finish(&self, cx: &StreamContext, status: &volo_streamx::StreamStatus) {
        self.check(cx);
        if cx.status().is_none() || self.closed.swap(true, Ordering::AcqRel) {
            self.counts.violations.fetch_add(1, Ordering::Relaxed);
        }
        self.counts.finished.fetch_add(1, Ordering::Relaxed);
        if status.sent_messages != self.sent.load(Ordering::Relaxed) as u64
            || status.received_messages != self.received.load(Ordering::Relaxed) as u64
        {
            self.counts.violations.fetch_add(1, Ordering::Relaxed);
        }
        self.counts.records.lock().unwrap().push(format!(
            "{}/{} {:?}: native send={} recv={}, hook send={} recv={}",
            cx.service(),
            cx.method(),
            cx.mode(),
            status.sent_messages,
            status.received_messages,
            self.sent.load(Ordering::Relaxed),
            self.received.load(Ordering::Relaxed)
        ));
    }
}
fn shared_observer(counts: &Arc<SharedHookCounts>, cx: &StreamContext) -> SharedObserver {
    let identity = counts.created.fetch_add(1, Ordering::Relaxed);
    assert!(cx.set_scope(Arc::new(FrameworkScope(identity))));
    cx.with_extensions(|extensions| {
        assert!(extensions.get::<HookIdentity>().is_none());
        extensions.insert(HookIdentity(identity));
    });
    SharedObserver {
        counts: counts.clone(),
        identity,
        closed: std::sync::atomic::AtomicBool::new(false),
        sent: AtomicUsize::new(0),
        received: AtomicUsize::new(0),
    }
}
#[derive(Clone)]
struct AdmitLayer(Arc<std::sync::atomic::AtomicBool>);
struct AdmitService<S>(S, Arc<std::sync::atomic::AtomicBool>);
impl<S> volo::Layer<S> for AdmitLayer {
    type Service = AdmitService<S>;
    fn layer(self, inner: S) -> Self::Service {
        AdmitService(inner, self.0)
    }
}
impl<'a, S> volo::Service<StreamContext, volo_streamx::Invocation<'a>> for AdmitService<S>
where
    S: volo::Service<
            StreamContext,
            volo_streamx::Invocation<'a>,
            Response = (),
            Error = volo_streamx::RpcError,
        > + Send
        + Sync,
{
    type Response = ();
    type Error = volo_streamx::RpcError;
    async fn call(
        &self,
        cx: &mut StreamContext,
        invocation: volo_streamx::Invocation<'a>,
    ) -> RpcResult<()> {
        if self.1.load(Ordering::Relaxed) {
            return Err(volo_streamx::RpcError::Biz {
                code: 401,
                message: "denied by global layer".into(),
                extra: Default::default(),
            });
        }
        self.0.call(cx, invocation).await
    }
}
impl<'a, S> volo::Service<StreamContext, volo_streamx::Invocation<'a>> for GlobalService<S>
where
    S: volo::Service<
            StreamContext,
            volo_streamx::Invocation<'a>,
            Response = (),
            Error = volo_streamx::RpcError,
        > + Send
        + Sync,
{
    type Response = ();
    type Error = volo_streamx::RpcError;
    async fn call(
        &self,
        cx: &mut StreamContext,
        invocation: volo_streamx::Invocation<'a>,
    ) -> RpcResult<()> {
        assert_eq!(StreamContext::current().unwrap().id(), cx.id());
        self.layer.entered.fetch_add(1, Ordering::Relaxed);
        let _exited = DropCount(self.layer.exited.clone());
        cx.with_extensions(|extensions| {
            if self.layer.tag == 1 {
                assert!(extensions.get::<LayerTrace>().is_none());
                extensions.insert(LayerTrace::default());
            }
            let trace = &mut extensions.get_mut::<LayerTrace>().unwrap().0;
            assert_eq!(trace.len(), usize::from(self.layer.tag - 1));
            trace.push(self.layer.tag);
        });
        self.inner.call(cx, invocation).await
    }
}

#[tokio::test]
async fn unified_server_serves_all_three_drivers_and_both_idls_on_one_address() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let shared = chat();
        let archived = Arc::new(AtomicUsize::new(0));
        let codec = CountCodec::default();
        let observed = codec.clone();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = events.clone();
        let entered = Arc::new(AtomicUsize::new(0));
        let exited = Arc::new(AtomicUsize::new(0));
        let hooks = Arc::new(SharedHookCounts::default());
        let observed_hooks = hooks.clone();
        let prepared = volo_streamx::Server::new()
            .add_fallback_service(chat_service_streamx::BoundService::from_arc(shared.clone()))
            .add_service(archive_service_streamx::bind(Archive(archived.clone())))
            .add_service(pb::presence_service_streamx::bind(Presence))
            .thrift(|server| server.make_codec(codec))
            .connection_observer(move |event| seen.lock().unwrap().push(event))
            .layer(GlobalLayer { tag: 1, entered: entered.clone(), exited: exited.clone() })
            .layer(GlobalLayer { tag: 2, entered: entered.clone(), exited: exited.clone() })
            .stream_hook(move |cx: &StreamContext| shared_observer(&observed_hooks, cx))
            .graceful_shutdown_timeout(Duration::from_secs(2))
            .prepare().unwrap();
        assert_eq!(prepared.protocols(), volo_streamx::Protocols::ALL);
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let thrift = ChatServiceClientBuilder::new("chat").address(address).build();
        let archive = ArchiveServiceClientBuilder::new("archive").address(address).build();
        let grpc = streamx_codegen::grpc::chat::ChatServiceClientBuilder::new("chat").address(address).build();
        let presence = pb::PresenceServiceClientBuilder::new("presence").address(address).build();
        assert!(matches!(thrift.get(message(7)).await.unwrap(), volo_thrift::MaybeException::Ok(value) if value.text == "hello@shared"));
        assert_eq!(archive.get(message(7)).await.unwrap().text, "archive");
        assert_eq!(thrift.add(3, 4).await.unwrap(), 7);
        for seq in [-1, -2] {
            assert!(matches!(thrift.get(message(seq)).await.unwrap(), volo_thrift::MaybeException::Exception(_)));
            assert!(grpc.get(streamx_codegen::grpc::chat::ChatServiceGetRequest { request: message(seq) }).await.unwrap().into_inner().is_err());
        }
        assert_eq!(grpc.get(streamx_codegen::grpc::chat::ChatServiceGetRequest { request: message(7) }).await.unwrap().into_inner().unwrap().text, "hello@shared");
        for seq in [0, -1, -2] { thrift.fire(message(seq)).await.unwrap(); thrift.ping().await.unwrap(); }
        let mut upload = thrift.upload().await.unwrap();
        upload.send(&message(0)).await.unwrap();
        upload.send(&message(1)).await.unwrap();
        assert_eq!(upload.close_and_recv().await.unwrap().sequence, 2);
        assert_eq!(grpc.upload(futures::stream::iter([message(0), message(1)])).await.unwrap().into_inner().sequence, 2);
        let mut watch = thrift.watch(message(0)).await.unwrap();
        for seq in 0..3 { assert_eq!(watch.recv().await.unwrap().unwrap().sequence, seq); }
        assert!(watch.recv().await.unwrap().is_none());
        let mut watch = grpc.watch(message(0)).await.unwrap().into_inner();
        for seq in 0..3 { assert_eq!(watch.message().await.unwrap().unwrap().sequence, seq); }
        assert!(watch.message().await.unwrap().is_none());
        let (mut send, mut recv) = thrift.talk().await.unwrap().into_split();
        send.send(&message(9)).await.unwrap();
        send.close_send().await.unwrap();
        assert_eq!(recv.recv().await.unwrap().unwrap().text, "hello@shared");
        assert!(recv.recv().await.unwrap().is_none());
        let mut talk = grpc.talk(futures::stream::iter([message(9)])).await.unwrap().into_inner();
        assert_eq!(talk.message().await.unwrap().unwrap().text, "hello@shared");
        assert!(talk.message().await.unwrap().is_none());
        let event = pb::Event { text: "protobuf".into(), sequence: 7 };
        assert_eq!(presence.get(event.clone()).await.unwrap().into_inner(), event);
        assert_eq!(presence.upload(futures::stream::iter([event.clone(), event.clone()])).await.unwrap().into_inner().sequence, 14);
        let mut watch = Box::pin(presence.watch(event.clone()).await.unwrap().into_inner());
        assert_eq!(watch.next().await.transpose().unwrap(), Some(event.clone()));
        assert!(watch.next().await.transpose().unwrap().is_none());
        let mut talk = Box::pin(presence.talk(futures::stream::iter([event.clone()])).await.unwrap().into_inner());
        assert_eq!(talk.next().await.transpose().unwrap(), Some(event));
        assert!(talk.next().await.transpose().unwrap().is_none());
        assert_eq!(shared.calls.load(Ordering::Relaxed), 12);
        assert_eq!(shared.fire.load(Ordering::Relaxed), 3);
        assert_eq!(archived.load(Ordering::Relaxed), 1);
        assert_eq!(entered.load(Ordering::Relaxed), 48);
        assert_eq!(exited.load(Ordering::Relaxed), 48);
        assert_eq!(hooks.created.load(Ordering::Relaxed), 24);
        assert_eq!(hooks.opened.load(Ordering::Relaxed), 24);
        assert_eq!(hooks.finished.load(Ordering::Relaxed), 24);
        assert_eq!(hooks.violations.load(Ordering::Relaxed), 0);
        assert_eq!(hooks.sent.load(Ordering::Relaxed), 17);
        assert_eq!(hooks.received.load(Ordering::Relaxed), 16, "{:#?}", hooks.records.lock().unwrap());
        assert_eq!(hooks.trailers.load(Ordering::Relaxed), 16);
        assert_eq!(observed.probes.load(Ordering::Relaxed), 0);
        assert_eq!(observed.unbuffered.load(Ordering::Relaxed), 0);
        let stats = handle.stats();
        assert!(stats.thrift >= 1 && stats.ttstream >= 1 && stats.grpc >= 1, "{stats:?}");
        handle.shutdown();
        server.await.unwrap().unwrap();
        let stats = handle.stats();
        assert_eq!((stats.active, stats.active_calls, stats.active_tasks), (0, 0, 0));
        let events = events.lock().unwrap();
        assert_eq!(events.iter().filter(|event| matches!(event, volo_streamx::ConnectionEvent::Selected { .. })).count(), stats.accepted);
        assert_eq!(events.iter().filter(|event| matches!(event, volo_streamx::ConnectionEvent::Closed(Some(_)))).count(), stats.accepted);
        assert_eq!(events.len(), 2 * stats.accepted);
    }).await.unwrap();
}

struct MustNotBind(Arc<AtomicUsize>);
impl volo::net::MakeIncoming for MustNotBind {
    type Incoming = DefaultIncoming;
    async fn make_incoming(self) -> std::io::Result<Self::Incoming> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(std::io::Error::other("unexpected listener creation"))
    }
}

struct PendingIncoming(Arc<AtomicUsize>);
impl volo::net::MakeIncoming for PendingIncoming {
    type Incoming = DefaultIncoming;
    async fn make_incoming(self) -> std::io::Result<Self::Incoming> {
        self.0.fetch_add(1, Ordering::Relaxed);
        futures::future::pending().await
    }
}

#[tokio::test]
async fn unified_shutdown_interrupts_listener_creation_and_bounds_shutdown_hooks() {
    tokio::time::timeout(Duration::from_secs(3), async {
        for early in [true, false] {
            let attempts = Arc::new(AtomicUsize::new(0));
            let hooks = Arc::new(AtomicUsize::new(0));
            let dropped = Arc::new(AtomicUsize::new(0));
            let entered = hooks.clone();
            let released = dropped.clone();
            let prepared = volo_streamx::Server::new()
                .add_service(chat_service_streamx::BoundService::from_arc(chat()))
                .thrift(|server| {
                    server.register_shutdown_hook(move || {
                        Box::pin(async move {
                            let _guard = DropCount(released);
                            entered.fetch_add(1, Ordering::Relaxed);
                            futures::future::pending().await
                        })
                    })
                })
                .graceful_shutdown_timeout(Duration::from_millis(30))
                .prepare()
                .unwrap();
            let handle = prepared.handle();
            if early {
                handle.shutdown();
            }
            let task = tokio::spawn(prepared.run_with_shutdown(
                PendingIncoming(attempts.clone()),
                futures::future::pending(),
            ));
            if !early {
                while attempts.load(Ordering::Relaxed) == 0 {
                    tokio::task::yield_now().await;
                }
                handle.shutdown();
            }
            task.await.unwrap().unwrap();
            assert_eq!(attempts.load(Ordering::Relaxed), usize::from(!early));
            assert_eq!(hooks.load(Ordering::Relaxed), 1);
            assert_eq!(dropped.load(Ordering::Relaxed), 1);
            assert_eq!(handle.stats().accepted, 0);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unified_configuration_errors_fail_before_make_incoming() {
    let calls = Arc::new(AtomicUsize::new(0));
    let bound = chat_service_streamx::BoundService::from_arc(chat());
    let invalid = [
        volo_streamx::Server::new()
            .add_service(bound.clone())
            .grpc(|server| server.accept_http1(true)),
        volo_streamx::Server::new()
            .add_service(bound.clone())
            .protocols(volo_streamx::Protocols::GRPC)
            .grpc(|server| server.accept_http1(true)),
        volo_streamx::Server::new()
            .add_service(bound.clone())
            .add_service(bound.clone()),
        volo_streamx::Server::new()
            .add_fallback_service(bound.clone())
            .add_fallback_service(bound.clone()),
        volo_streamx::Server::new()
            .add_service(bound.clone())
            .add_service(pb::presence_service_streamx::bind(Presence)),
        volo_streamx::Server::new()
            .add_service(bound.clone())
            .protocols(volo_streamx::Protocols::THRIFT & volo_streamx::Protocols::GRPC),
        volo_streamx::Server::new()
            .add_service(pb::presence_service_streamx::bind(Presence))
            .protocols(volo_streamx::Protocols::THRIFT),
        volo_streamx::Server::new()
            .add_service(bound.clone())
            .grpc(|server| {
                server.add_service(volo_grpc::server::ServiceBuilder::new(bound.grpc()).build())
            }),
    ];
    for (index, server) in invalid.into_iter().enumerate() {
        assert!(
            server
                .run_with_shutdown(MustNotBind(calls.clone()), futures::future::pending())
                .await
                .is_err()
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "invalid configuration {index}"
        );
    }
    let server = volo_streamx::Server::new()
        .add_service(bound)
        .thrift(|server| {
            server.make_codec(CountCodec {
                reject_buffered: true,
                ..Default::default()
            })
        });
    assert!(
        server
            .run_with_shutdown(MustNotBind(calls.clone()), futures::future::pending())
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn unified_single_driver_entries_bypass_detection_and_native_probe() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for protocols in [
            volo_streamx::Protocols::THRIFT,
            volo_streamx::Protocols::TTSTREAM,
            volo_streamx::Protocols::GRPC,
        ] {
            let shared = chat();
            let codec = CountCodec::default();
            let observed = codec.clone();
            let prepared = volo_streamx::Server::new()
                .add_service(chat_service_streamx::BoundService::from_arc(shared))
                .protocols(protocols)
                .thrift(|server| server.make_codec(codec))
                .graceful_shutdown_timeout(Duration::from_secs(1))
                .prepare()
                .unwrap();
            let handle = prepared.handle();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
            if protocols == volo_streamx::Protocols::GRPC {
                let client = streamx_codegen::grpc::chat::ChatServiceClientBuilder::new("chat")
                    .address(address)
                    .build();
                client
                    .get(streamx_codegen::grpc::chat::ChatServiceGetRequest {
                        request: message(7),
                    })
                    .await
                    .unwrap();
            } else {
                let client = ChatServiceClientBuilder::new("chat")
                    .address(address)
                    .build();
                if protocols == volo_streamx::Protocols::THRIFT {
                    client.ping().await.unwrap();
                } else {
                    let mut stream = client.watch(message(1)).await.unwrap();
                    while stream.recv().await.unwrap().is_some() {}
                }
            }
            assert_eq!(handle.stats().detected, 0);
            assert_eq!(observed.probes.load(Ordering::Relaxed), 0);
            handle.shutdown();
            task.await.unwrap().unwrap();
            assert_eq!(handle.stats().active, 0);
        }
    })
    .await
    .unwrap();
}

fn ordinary_request(oneway: bool, text: &str, framed: bool) -> bytes::Bytes {
    use pilota::thrift::{Message as _, TOutputProtocol};
    let mut payload = pilota::LinkedBytes::new();
    let mut protocol = pilota::thrift::binary::TBinaryProtocol::new(&mut payload, true);
    protocol
        .write_message_begin(&pilota::thrift::TMessageIdentifier::new(
            if oneway { "Fire" } else { "Get" }.into(),
            if oneway {
                pilota::thrift::TMessageType::OneWay
            } else {
                pilota::thrift::TMessageType::Call
            },
            131,
        ))
        .unwrap();
    let request = Message {
        text: text.to_owned().into(),
        sequence: 0,
    };
    if oneway {
        ChatServiceFireArgsRecv { request }
            .encode(&mut protocol)
            .unwrap();
    } else {
        ChatServiceGetArgsRecv { request }
            .encode(&mut protocol)
            .unwrap();
    }
    protocol.write_message_end().unwrap();
    let payload = payload.into_bytes_mut().freeze();
    if framed {
        let mut bytes = (payload.len() as i32).to_be_bytes().to_vec();
        bytes.extend_from_slice(&payload);
        bytes.into()
    } else {
        payload
    }
}

#[tokio::test]
async fn unified_idle_and_short_prefix_connections_do_not_block_accept_or_shutdown() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::time::timeout(Duration::from_secs(5), async {
        let shared = chat();
        let prepared = volo_streamx::Server::new()
            .add_service(chat_service_streamx::BoundService::from_arc(shared.clone()))
            .graceful_shutdown_timeout(Duration::from_secs(1))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let mut idle = tokio::net::TcpStream::connect(address).await.unwrap();
        for size in 0..8 {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream.write_all(&b"PRI * HT"[..size]).await.unwrap();
            stream.shutdown().await.unwrap();
        }
        let client = ChatServiceClientBuilder::new("chat")
            .address(address)
            .build();
        client.get(message(7)).await.unwrap();
        assert_eq!(shared.calls.load(Ordering::Relaxed), 1);
        while handle.stats().rejected != 8 {
            tokio::task::yield_now().await;
        }
        handle.shutdown();
        task.await.unwrap().unwrap();
        assert_eq!(
            idle.read_u8().await.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        assert_eq!(handle.stats().active, 0);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unified_drain_retains_disconnected_framed_unary_and_oneway_until_commit() {
    use tokio::io::AsyncWriteExt;
    tokio::time::timeout(Duration::from_secs(5), async {
        for oneway in [false, true] {
            let started = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let shared = Arc::new(Chat {
                calls: AtomicUsize::new(0),
                fire: AtomicUsize::new(0),
                barriers: Some((started.clone(), release.clone())),
            });
            let server = volo_streamx::Server::new()
                .add_service(chat_service_streamx::BoundService::from_arc(shared.clone()))
                .protocols(volo_streamx::Protocols::THRIFT)
                .graceful_shutdown_timeout(Duration::from_secs(1));
            #[cfg(feature = "multiplex")]
            let server = server.thrift(|server| server.multiplex(true));
            let prepared = server.prepare().unwrap();
            let handle = prepared.handle();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream
                .write_all(&ordinary_request(oneway, "block", true))
                .await
                .unwrap();
            started.notified().await;
            drop(stream);
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(handle.stats().active_calls, 1);
            assert_eq!(
                handle.stats().active,
                1,
                "framed multiplex work must retain its connection owner after EOF"
            );
            handle.shutdown();
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(!task.is_finished());
            release.notify_one();
            task.await.unwrap().unwrap();
            assert_eq!(shared.calls.load(Ordering::Relaxed), usize::from(!oneway));
            assert_eq!(shared.fire.load(Ordering::Relaxed), usize::from(oneway));
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

#[derive(Default)]
struct StopState {
    contexts: std::sync::Mutex<Vec<StreamContext>>,
    changed: tokio::sync::Notify,
    dropped: Arc<AtomicUsize>,
}
struct DropCount(Arc<AtomicUsize>);
impl Drop for DropCount {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl StopState {
    async fn hold<T>(self: &Arc<Self>, context: StreamContext) -> RpcResult<T> {
        check_framework_scope(&context);
        let _handler = DropCount(self.dropped.clone());
        let started = Arc::new(tokio::sync::Notify::new());
        let child_started = started.clone();
        let dropped = self.dropped.clone();
        let identity =
            context.with_extensions(|extensions| extensions.get::<HookIdentity>().map(|id| id.0));
        let child = context.spawn_scoped(async move {
            if let Some(identity) = identity {
                assert_eq!(HOOK_SCOPE.try_with(|scope| *scope).ok(), Some(identity));
            }
            let _child = DropCount(dropped);
            child_started.notify_one();
            futures::future::pending::<()>().await;
        });
        drop(child);
        started.notified().await;
        self.contexts.lock().unwrap().push(context);
        self.changed.notify_waiters();
        futures::future::pending().await
    }
    async fn wait(&self, count: usize) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.contexts.lock().unwrap().len() == count {
                return;
            }
            changed.await;
        }
    }
}
struct StopChat(Arc<StopState>);
impl chat_service_streamx::Handler for StopChat {
    async fn get(
        &self,
        cx: StreamContext,
        _: Message,
    ) -> RpcResult<Result<Message, ChatServiceGetException>> {
        self.0.hold(cx).await
    }
    async fn watch(
        &self,
        cx: StreamContext,
        _: Message,
        _: ResponseSender<Message>,
    ) -> RpcResult<()> {
        self.0.hold(cx).await
    }
    async fn fire(&self, cx: StreamContext, _: Message) -> RpcResult<()> {
        self.0.hold(cx).await
    }
    async fn ping(&self, _: StreamContext, _: ()) -> RpcResult<()> {
        Ok(())
    }
    async fn add(&self, _: StreamContext, _: ChatServiceAddArgsRecv) -> RpcResult<i64> {
        unreachable!()
    }
    async fn upload(&self, _: StreamContext, _: RequestStream<Message>) -> RpcResult<Message> {
        unreachable!()
    }
    async fn talk(
        &self,
        _: StreamContext,
        _: RequestStream<Message>,
        _: ResponseSender<Message>,
    ) -> RpcResult<()> {
        unreachable!()
    }
}
struct StopPresence(Arc<StopState>);
impl pb::presence_service_streamx::Handler for StopPresence {
    async fn get(&self, cx: StreamContext, _: pb::Event) -> RpcResult<pb::Event> {
        self.0.hold(cx).await
    }
    async fn upload(&self, _: StreamContext, _: RequestStream<pb::Event>) -> RpcResult<pb::Event> {
        unreachable!()
    }
    async fn watch(
        &self,
        _: StreamContext,
        _: pb::Event,
        _: ResponseSender<pb::Event>,
    ) -> RpcResult<()> {
        unreachable!()
    }
    async fn talk(
        &self,
        _: StreamContext,
        _: RequestStream<pb::Event>,
        _: ResponseSender<pb::Event>,
    ) -> RpcResult<()> {
        unreachable!()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unified_deadline_reclaims_all_protocols_with_the_server_cancellation_cause() {
    assert_unified_shutdown(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unified_run_drop_reclaims_all_protocols_with_the_server_cancellation_cause() {
    assert_unified_shutdown(true).await;
}

async fn assert_unified_shutdown(abort: bool) {
    use tokio::io::AsyncWriteExt;
    tokio::time::timeout(Duration::from_secs(8), async {
        let state = Arc::new(StopState::default());
        let layer_entered = Arc::new(AtomicUsize::new(0));
        let layer_exited = Arc::new(AtomicUsize::new(0));
        let hooks = Arc::new(SharedHookCounts::default());
        let observed_hooks = hooks.clone();
        let server = volo_streamx::Server::new()
            .add_fallback_service(chat_service_streamx::bind(StopChat(state.clone())))
            .add_service(pb::presence_service_streamx::bind(StopPresence(
                state.clone(),
            )))
            .layer(GlobalLayer {
                tag: 1,
                entered: layer_entered.clone(),
                exited: layer_exited.clone(),
            })
            .layer(GlobalLayer {
                tag: 2,
                entered: layer_entered.clone(),
                exited: layer_exited.clone(),
            })
            .stream_hook(move |cx: &StreamContext| shared_observer(&observed_hooks, cx))
            .thrift(|server| {
                server.streaming_config(volo_thrift::streaming::StreamConfig {
                    drain_timeout: Duration::from_millis(1),
                    ..Default::default()
                })
            })
            .graceful_shutdown_timeout(Duration::from_millis(100));
        #[cfg(feature = "multiplex")]
        let server = server.thrift(|server| server.multiplex(true));
        let prepared = server.prepare().unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let mut ordinary = tokio::net::TcpStream::connect(address).await.unwrap();
        ordinary
            .write_all(&ordinary_request(false, "stall", true))
            .await
            .unwrap();
        let client = ChatServiceClientBuilder::new("chat")
            .address(address)
            .build();
        let natives: Vec<_> = (0..16)
            .map(|_| {
                let client = client.clone();
                tokio::spawn(async move {
                    match client.watch(message(0)).await {
                        Ok(mut stream) => stream.recv().await.is_err(),
                        Err(_) => true,
                    }
                })
            })
            .collect();
        let grpc = tokio::spawn(async move {
            streamx_codegen::grpc::chat::ChatServiceClientBuilder::new("chat")
                .address(address)
                .build()
                .get(streamx_codegen::grpc::chat::ChatServiceGetRequest {
                    request: message(0),
                })
                .await
        });
        let proto = tokio::spawn(async move {
            pb::PresenceServiceClientBuilder::new("presence")
                .address(address)
                .build()
                .get(pb::Event::default())
                .await
        });
        state.wait(19).await;
        assert_eq!(handle.stats().active_calls, 19);
        drop(ordinary);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            state
                .contexts
                .lock()
                .unwrap()
                .iter()
                .all(|cx| cx.cancellation().is_none())
        );
        let stopped_at = std::time::Instant::now();
        if abort {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            loop {
                let stats = handle.stats();
                if stats.active == 0 && stats.active_calls == 0 && stats.active_tasks == 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        } else {
            handle.shutdown();
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(!task.is_finished());
            task.await.unwrap().unwrap();
            assert!(stopped_at.elapsed() >= Duration::from_millis(80));
        }
        for native in natives {
            assert!(native.await.unwrap());
        }
        assert!(grpc.await.unwrap().is_err());
        assert!(proto.await.unwrap().is_err());
        let stats = handle.stats();
        assert_eq!(
            (stats.active, stats.active_calls, stats.active_tasks),
            (0, 0, 0)
        );
        assert_eq!(state.dropped.load(Ordering::SeqCst), 38);
        assert_eq!(layer_entered.load(Ordering::Relaxed), 38);
        assert_eq!(layer_exited.load(Ordering::Relaxed), 38);
        assert_eq!(hooks.created.load(Ordering::Relaxed), 19);
        assert_eq!(hooks.opened.load(Ordering::Relaxed), 19);
        assert_eq!(hooks.finished.load(Ordering::Relaxed), 19);
        assert_eq!(hooks.violations.load(Ordering::Relaxed), 0);
        for cx in state.contexts.lock().unwrap().iter() {
            assert!(
                matches!(
                    cx.cancellation(),
                    Some(volo_streamx::RpcError::Canceled {
                        cause: volo_streamx::CancelCause::ServerShutdown,
                        ..
                    })
                ),
                "{} {:?}",
                cx.service(),
                cx.cancellation()
            );
            fn server_cause(error: &volo_streamx::RpcError) -> bool {
                match error {
                    volo_streamx::RpcError::Canceled {
                        cause: volo_streamx::CancelCause::ServerShutdown,
                        ..
                    } => true,
                    volo_streamx::RpcError::GrpcStatus(status) => status
                        .source
                        .as_deref()
                        .and_then(|source| source.downcast_ref::<volo_streamx::RpcError>())
                        .is_some_and(server_cause),
                    _ => false,
                }
            }
            assert!(
                cx.status()
                    .and_then(|status| status.error)
                    .as_ref()
                    .is_some_and(server_cause),
                "{} {:?}",
                cx.service(),
                cx.status()
            );
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unified_h2_socket_keeps_both_payloads_and_strict_policy_during_other_streams() {
    tokio::time::timeout(Duration::from_secs(8), async {
        let prepared = volo_streamx::Server::new()
            .add_fallback_service(chat_service_streamx::BoundService::from_arc(chat()))
            .add_service(pb::presence_service_streamx::bind(Presence))
            .graceful_shutdown_timeout(Duration::from_secs(1))
            .prepare()
            .unwrap();
        let directory = prepared.directory().clone();
        let selected = directory.find_thrift("", "Get").unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let (mut client, connection) =
            h2::client::handshake(tokio::net::TcpStream::connect(address).await.unwrap())
                .await
                .unwrap();
        let driver = tokio::spawn(connection);
        let (thrift, mut thrift_input) = raw_request(
            &mut client,
            address,
            "/ChatService/Talk",
            &["application/grpc+thrift"],
            thrift_bytes(&message(7)),
            false,
        );
        let mut thrift = thrift.await.unwrap().into_body();
        let bytes = thrift.data().await.unwrap().unwrap();
        thrift.flow_control().release_capacity(bytes.len()).unwrap();
        let event = pb::Event {
            text: "one-socket".into(),
            sequence: 19,
        };
        let payload: bytes::Bytes =
            pilota::pb::Message::encode_to_vec(&event, &mut Default::default()).into();
        let (proto, mut proto_input) = raw_request(
            &mut client,
            address,
            "/streamx.presence.PresenceService/Talk",
            &["application/grpc"],
            payload.clone(),
            false,
        );
        assert_ne!(thrift_input.stream_id(), proto_input.stream_id());
        let mut proto = proto.await.unwrap().into_body();
        let bytes = proto.data().await.unwrap().unwrap();
        proto.flow_control().release_capacity(bytes.len()).unwrap();
        let (bad, _) = raw_request(
            &mut client,
            address,
            "/streamx.presence.PresenceService/Get",
            &[],
            payload.clone(),
            true,
        );
        assert_eq!(
            raw_response(bad).await.0,
            http::StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        let (bad, _) = raw_request(
            &mut client,
            address,
            "/streamx.presence.PresenceService/Get",
            &["application/grpc+thrift"],
            payload.clone(),
            true,
        );
        assert_eq!(raw_response(bad).await.1["grpc-status"], "12");
        let (healthy, _) = raw_request(
            &mut client,
            address,
            "/streamx.presence.PresenceService/Get",
            &["application/grpc"],
            payload,
            true,
        );
        assert_eq!(raw_response(healthy).await.1["grpc-status"], "0");
        let request = ChatServiceGetArgsRecv {
            request: Message {
                text: "path-proof".into(),
                sequence: 0,
            },
        };
        let (alias, _) = raw_request(
            &mut client,
            address,
            "/other.package.ChatService/Get",
            &["application/grpc+thrift"],
            thrift_bytes(&request),
            true,
        );
        assert_eq!(raw_response(alias).await.1["grpc-status"], "0");
        assert!(Arc::ptr_eq(
            &selected,
            &directory.find_thrift("", "Get").unwrap()
        ));
        assert!(directory.find_thrift("wrong", "Get").is_none());
        thrift_input.send_data(bytes::Bytes::new(), true).unwrap();
        proto_input.send_data(bytes::Bytes::new(), true).unwrap();
        for mut body in [thrift, proto] {
            while let Some(bytes) = body.data().await {
                let bytes = bytes.unwrap();
                body.flow_control().release_capacity(bytes.len()).unwrap();
            }
            assert_eq!(body.trailers().await.unwrap().unwrap()["grpc-status"], "0");
        }
        assert_eq!(handle.stats().accepted, 1);
        assert_eq!(handle.stats().detected, 1);
        assert_eq!(handle.stats().grpc, 1);
        handle.shutdown();
        drop(client);
        server.await.unwrap().unwrap();
        driver.await.unwrap().unwrap();
        assert_eq!(
            (handle.stats().active_calls, handle.stats().active_tasks),
            (0, 0)
        );
    })
    .await
    .unwrap();
}

#[cfg(feature = "rustls")]
fn tls_config(protocols: &[&[u8]]) -> volo::net::tls::ServerTlsConfig {
    use tokio_rustls::rustls;
    let certificates = rustls_pemfile::certs(
        &mut include_bytes!("../../../examples/data/tls/server.pem").as_slice(),
    )
    .collect::<Result<Vec<_>, _>>()
    .unwrap();
    let key = rustls_pemfile::private_key(
        &mut include_bytes!("../../../examples/data/tls/server.key").as_slice(),
    )
    .unwrap()
    .unwrap();
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_protocol_versions(rustls::DEFAULT_VERSIONS)
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(certificates, key)
    .unwrap();
    config.alpn_protocols = protocols.iter().map(|protocol| protocol.to_vec()).collect();
    volo::net::tls::ServerTlsConfig {
        acceptor: config.into(),
    }
}
#[cfg(feature = "rustls")]
fn tls_connector(protocols: &[&str]) -> volo::net::tls::TlsConnector {
    volo::net::tls::TlsConnectorBuilder::new()
        .enable_default_root_certs(false)
        .add_pem(include_bytes!("../../../examples/data/tls/ca.pem").to_vec())
        .with_alpn_protocols(protocols.iter().copied())
        .build()
        .unwrap()
}

#[cfg(feature = "rustls")]
mod tls_connection_tests {
    use std::io;

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };
    use volo::net::{
        Address,
        conn::{Conn, ConnInfo, PeekedConn},
    };

    fn acceptor(protocols: Vec<Vec<u8>>) -> volo::net::tls::TlsAcceptor {
        let protocols = protocols.iter().map(Vec::as_slice).collect::<Vec<_>>();
        super::tls_config(&protocols).acceptor
    }

    #[test]
    fn streamx_rejects_http1_and_other_advertised_alpn_protocols() {
        for protocols in [vec![], vec![b"h2".to_vec()]] {
            acceptor(protocols).validate_streamx().unwrap();
        }
        for protocols in [
            vec![b"http/1.1".to_vec()],
            vec![b"h2".to_vec(), b"http/1.1".to_vec()],
            vec![b"custom".to_vec()],
        ] {
            assert_eq!(
                acceptor(protocols).validate_streamx().unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn thrift_only_server_rejects_advertised_h2() {
        let result = volo_streamx::Server::new()
            .add_service(super::chat_service_streamx::BoundService::from_arc(
                super::chat(),
            ))
            .protocols(volo_streamx::Protocols::THRIFT)
            .tls_config(super::tls_config(&[b"h2"]))
            .prepare();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn stalled_tls_handshake_is_rejected_at_the_connection_deadline() {
        use std::{
            sync::{Arc, Mutex},
            time::Duration,
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            let events = Arc::new(Mutex::new(Vec::new()));
            let observed = events.clone();
            let prepared = volo_streamx::Server::new()
                .add_service(super::chat_service_streamx::BoundService::from_arc(
                    super::chat(),
                ))
                .tls_config(super::tls_config(&[]))
                .connection_timeout(Duration::from_millis(30))
                .connection_observer(move |event| observed.lock().unwrap().push(event))
                .prepare()
                .unwrap();
            let handle = prepared.handle();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server =
                tokio::spawn(prepared.run(volo::net::incoming::DefaultIncoming::from(listener)));
            let mut socket = TcpStream::connect(address).await.unwrap();
            assert_eq!(socket.read(&mut [0; 1]).await.unwrap(), 0);
            assert!(events.lock().unwrap().iter().any(|event| matches!(
                event,
                volo_streamx::ConnectionEvent::Rejected(volo_streamx::Rejection::Timeout)
            )));
            handle.shutdown();
            server.await.unwrap().unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn detected_tls_keeps_the_session_peer_and_alpn_before_and_after_split() {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for alpn in [None, Some(b"h2".to_vec())] {
                let acceptor = acceptor(alpn.clone().into_iter().collect());
                let connector = super::tls_connector(&["h2"]);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let expected_alpn = alpn.clone();
                let server = tokio::spawn(async move {
                    let (socket, _) = listener.accept().await.unwrap();
                    let stream = acceptor.accept(socket).await.unwrap();
                    let peer = Address::Ip("192.0.2.2:1234".parse().unwrap());
                    let original = Conn::new(
                        stream,
                        ConnInfo {
                            peer_addr: Some(peer.clone()),
                        },
                    );
                    let mut conn = PeekedConn::peek8(original).await.unwrap().unwrap();
                    assert!(conn.is_tls());
                    assert_eq!(conn.info().peer_addr, Some(peer.clone()));
                    assert_eq!(conn.negotiated_alpn(), expected_alpn);
                    assert_eq!(conn.prefix(), b"PRI * HT");
                    let mut received = [0; 12];
                    if expected_alpn.is_some() {
                        let mut parts = conn.into_buffered_split(512);
                        assert!(parts.is_tls);
                        assert_eq!(parts.info.peer_addr, Some(peer));
                        assert_eq!(parts.negotiated_alpn, expected_alpn);
                        parts.reader.read_exact(&mut received).await.unwrap();
                        parts.writer.write_all(b"reply").await.unwrap();
                        parts.writer.shutdown().await.unwrap();
                    } else {
                        conn.read_exact(&mut received).await.unwrap();
                        conn.write_all(b"reply").await.unwrap();
                        conn.shutdown().await.unwrap();
                    }
                    assert_eq!(&received, b"PRI * HTtail");
                });
                let socket = TcpStream::connect(address).await.unwrap();
                let mut client = connector.connect("localhost", socket).await.unwrap();
                client.write_all(b"PRI * HTtail").await.unwrap();
                let mut response = [0; 5];
                client.read_exact(&mut response).await.unwrap();
                assert_eq!(&response, b"reply");
                server.await.unwrap();
            }
        })
        .await
        .expect("TLS replay exceeded its timeout");
    }
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn unified_tls_shares_one_config_and_checks_alpn_in_mixed_and_single_driver_modes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::time::timeout(Duration::from_secs(10), async {
        for alpn in [false, true] {
            for protocols in [volo_streamx::Protocols::ALL, volo_streamx::Protocols::GRPC] {
                let shared = chat();
                let prepared = volo_streamx::Server::new().add_fallback_service(chat_service_streamx::BoundService::from_arc(shared.clone()))
                    .add_service(pb::presence_service_streamx::bind(Presence)).protocols(protocols)
                    .tls_config(tls_config(if alpn { &[b"h2"] } else { &[] })).graceful_shutdown_timeout(Duration::from_secs(1)).prepare().unwrap();
                let handle = prepared.handle();
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
                let tls = tls_connector(&["h2"]).connect("localhost", tokio::net::TcpStream::connect(address).await.unwrap()).await.unwrap();
                assert_eq!(tls.negotiated_alpn(), alpn.then(|| b"h2".to_vec()));
                let (mut client, connection) = h2::client::handshake(tls).await.unwrap();
                let driver = tokio::spawn(connection);
                let payload: bytes::Bytes = pilota::pb::Message::encode_to_vec(&pb::Event::default(), &mut Default::default()).into();
                let (response, _) = raw_request(&mut client, address, "/streamx.presence.PresenceService/Get", &["application/grpc"], payload, true);
                assert_eq!(raw_response(response).await.1["grpc-status"], "0");
                let request = ChatServiceGetArgsRecv { request: message(7) };
                let (response, _) = raw_request(&mut client, address, "/ChatService/Get", &["application/grpc+thrift"], thrift_bytes(&request), true);
                assert_eq!(raw_response(response).await.1["grpc-status"], "0");
                if protocols == volo_streamx::Protocols::ALL {
                    let mut tls = tls_connector(&[]).connect("localhost", tokio::net::TcpStream::connect(address).await.unwrap()).await.unwrap();
                    tls.write_all(&ordinary_request(false, "hello", false)).await.unwrap();
                    let mut protocol = pilota::thrift::TAsyncBinaryProtocol::new(&mut tls);
                    use pilota::thrift::{Message as _, TAsyncInputProtocol};
                    let ident = protocol.read_message_begin().await.unwrap();
                    assert_eq!(ident.sequence_number, 131);
                    assert!(matches!(ChatServiceGetResultRecv::decode_async(&mut protocol).await.unwrap(), ChatServiceGetResultRecv::Ok(value) if value.text == "hello@shared"));
                    drop(tls);
                    use volo_thrift::transport::ttstream::frame::{Frame, FrameType, read_frame};
                    let config = volo_thrift::streaming::StreamConfig::default();
                    let mut tls = tls_connector(&[]).connect("localhost", tokio::net::TcpStream::connect(address).await.unwrap()).await.unwrap();
                    for (kind, payload) in [(FrameType::Header, bytes::Bytes::new()), (FrameType::Data, thrift_bytes(&message(7))), (FrameType::Trailer, bytes::Bytes::new())] {
                        tls.write_all(&Frame::new(-17, kind, bytes::Bytes::from_static(b"Watch"), payload).encode(&config).unwrap()).await.unwrap();
                    }
                    let mut messages = 0;
                    loop {
                        let frame = read_frame(&mut tls, &config).await.unwrap().unwrap();
                        assert_eq!(frame.stream_id, -17);
                        match frame.kind {
                            FrameType::Header => {},
                            FrameType::Data => {
                                let mut payload = frame.payload;
                                let value = Message::decode(&mut pilota::thrift::binary::TBinaryProtocol::new(&mut payload, true)).unwrap();
                                assert_eq!(value.sequence, messages); messages += 1;
                            }
                            FrameType::Trailer => { assert!(frame.payload.is_empty()); break; },
                            other => panic!("unexpected TTStream frame {other:?}"),
                        }
                    }
                    assert_eq!(messages, 3);
                    drop(tls);
                    assert_eq!(shared.calls.load(Ordering::Relaxed), 5);
                    if alpn {
                        let mut tls = tls_connector(&["h2"]).connect("localhost", tokio::net::TcpStream::connect(address).await.unwrap()).await.unwrap();
                        tls.write_all(&ordinary_request(false, "hello", false)).await.unwrap();
                        let mut closed = Vec::new(); let _ = tls.read_to_end(&mut closed).await;
                        assert!(closed.is_empty());
                        assert_eq!(handle.stats().rejected, 1);
                        assert_eq!(shared.calls.load(Ordering::Relaxed), 5);
                    }
                }
                assert_eq!(handle.stats().grpc, 1);
                if protocols == volo_streamx::Protocols::GRPC { assert_eq!(handle.stats().detected, 0); }
                handle.shutdown(); drop(client);
                server.await.unwrap().unwrap(); driver.await.unwrap().unwrap();
                assert_eq!((handle.stats().active, handle.stats().active_tasks), (0, 0));
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        for protocols in [volo_streamx::Protocols::ALL, volo_streamx::Protocols::GRPC] {
            let server = volo_streamx::Server::new().add_service(chat_service_streamx::BoundService::from_arc(chat()))
                .protocols(protocols).tls_config(tls_config(&[b"h2", b"http/1.1"]));
            assert!(server.run_with_shutdown(MustNotBind(calls.clone()), futures::future::pending()).await.is_err());
            assert_eq!(calls.load(Ordering::Relaxed), 0);
        }
    }).await.unwrap();
}

struct OnceIncoming(Option<volo::net::conn::Conn>);
impl std::fmt::Debug for OnceIncoming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OnceIncoming")
    }
}
impl volo::net::MakeIncoming for OnceIncoming {
    type Incoming = Self;
    async fn make_incoming(self) -> std::io::Result<Self> {
        Ok(self)
    }
}
impl volo::net::incoming::Incoming for OnceIncoming {
    async fn accept(&mut self) -> std::io::Result<Option<volo::net::conn::Conn>> {
        match self.0.take() {
            Some(conn) => Ok(Some(conn)),
            None => futures::future::pending().await,
        }
    }
}

#[cfg(all(feature = "rustls", target_family = "unix"))]
#[tokio::test]
async fn unified_tls_rejects_known_uds_before_bind_and_custom_uds_before_business() {
    use tokio::io::AsyncReadExt;
    tokio::time::timeout(Duration::from_secs(5), async {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("must-not-bind.sock");
        let address =
            volo::net::Address::Unix(std::os::unix::net::SocketAddr::from_pathname(&path).unwrap());
        let server = volo_streamx::Server::new()
            .add_service(chat_service_streamx::BoundService::from_arc(chat()))
            .tls_config(tls_config(&[]));
        assert!(
            server
                .run_with_shutdown(address, futures::future::pending())
                .await
                .is_err()
        );
        assert!(!path.exists());
        let shared = chat();
        let prepared = volo_streamx::Server::new()
            .add_service(chat_service_streamx::BoundService::from_arc(shared.clone()))
            .tls_config(tls_config(&[]))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let (mut client, accepted) = tokio::net::UnixStream::pair().unwrap();
        let server = tokio::spawn(prepared.run(OnceIncoming(Some(accepted.into()))));
        assert_eq!(
            client.read_u8().await.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        assert_eq!(handle.stats().rejected, 1);
        assert_eq!(shared.calls.load(Ordering::Relaxed), 0);
        handle.shutdown();
        server.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}

#[cfg(target_family = "unix")]
#[tokio::test]
async fn unified_uds_listener_supports_thrift_ttstream_and_both_grpc_payloads() {
    tokio::time::timeout(Duration::from_secs(8), async {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("mixed.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let address =
            volo::net::Address::Unix(std::os::unix::net::SocketAddr::from_pathname(&path).unwrap());
        let prepared = volo_streamx::Server::new()
            .add_fallback_service(chat_service_streamx::BoundService::from_arc(chat()))
            .add_service(pb::presence_service_streamx::bind(Presence))
            .graceful_shutdown_timeout(Duration::from_secs(1))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let thrift = ChatServiceClientBuilder::new("chat")
            .address(address.clone())
            .build();
        thrift.get(message(7)).await.unwrap();
        let mut stream = thrift.watch(message(7)).await.unwrap();
        let mut count = 0;
        while stream.recv().await.unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 3);
        streamx_codegen::grpc::chat::ChatServiceClientBuilder::new("chat")
            .address(address.clone())
            .build()
            .get(streamx_codegen::grpc::chat::ChatServiceGetRequest {
                request: message(7),
            })
            .await
            .unwrap();
        pb::PresenceServiceClientBuilder::new("presence")
            .address(address)
            .build()
            .get(pb::Event::default())
            .await
            .unwrap();
        let stats = handle.stats();
        assert!(
            stats.thrift >= 1 && stats.ttstream >= 1 && stats.grpc >= 1,
            "{stats:?}"
        );
        handle.shutdown();
        server.await.unwrap().unwrap();
        assert_eq!((handle.stats().active, handle.stats().active_tasks), (0, 0));
    })
    .await
    .unwrap();
}

#[derive(Clone)]
struct PeerLayer(volo::net::Address);
#[derive(Clone)]
struct PeerService<S> {
    inner: S,
    expected: volo::net::Address,
}
impl<S> volo::Layer<S> for PeerLayer {
    type Service = PeerService<S>;
    fn layer(self, inner: S) -> Self::Service {
        PeerService {
            inner,
            expected: self.0,
        }
    }
}
impl<S, R: Send + 'static> volo::Service<volo_thrift::context::ServerContext, R> for PeerService<S>
where
    S: volo::Service<volo_thrift::context::ServerContext, R> + Send + Sync,
{
    type Response = S::Response;
    type Error = S::Error;
    async fn call(
        &self,
        cx: &mut volo_thrift::context::ServerContext,
        request: R,
    ) -> Result<Self::Response, Self::Error> {
        assert_eq!(cx.rpc_info.caller().address(), Some(self.expected.clone()));
        self.inner.call(cx, request).await
    }
}
impl<S>
    volo::Service<volo_grpc::context::ServerContext, volo_grpc::Request<volo_grpc::body::BoxBody>>
    for PeerService<S>
where
    S: volo::Service<
            volo_grpc::context::ServerContext,
            volo_grpc::Request<volo_grpc::body::BoxBody>,
        > + Send
        + Sync,
{
    type Response = S::Response;
    type Error = S::Error;
    async fn call(
        &self,
        cx: &mut volo_grpc::context::ServerContext,
        request: volo_grpc::Request<volo_grpc::body::BoxBody>,
    ) -> Result<Self::Response, Self::Error> {
        assert_eq!(cx.rpc_info.caller().address(), Some(self.expected.clone()));
        self.inner.call(cx, request).await
    }
}

#[tokio::test]
async fn unified_custom_incoming_keeps_peer_info_layers_and_existing_tls_sessions() {
    use tokio::io::AsyncWriteExt;
    tokio::time::timeout(Duration::from_secs(8), async {
        let cases: &[bool] = if cfg!(feature = "rustls") {
            &[false, true]
        } else {
            &[false]
        };
        for &tls in cases {
            for grpc in [false, true] {
                let peer = volo::net::Address::Ip("192.0.2.55:1731".parse().unwrap());
                let server = volo_streamx::Server::new()
                    .add_fallback_service(chat_service_streamx::BoundService::from_arc(chat()))
                    .add_service(pb::presence_service_streamx::bind(Presence))
                    .thrift(|server| server.layer(PeerLayer(peer.clone())))
                    .grpc(|server| server.layer(PeerLayer(peer.clone())))
                    .graceful_shutdown_timeout(Duration::from_secs(1));
                #[cfg(feature = "rustls")]
                let server = if tls {
                    server.tls_config(tls_config(&[b"h2"]))
                } else {
                    server
                };
                let prepared = server.prepare().unwrap();
                let handle = prepared.handle();
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let (mut client, accepted): (volo::net::conn::Conn, volo::net::conn::Conn) = if tls
                {
                    #[cfg(feature = "rustls")]
                    {
                        let acceptor = tls_config(&[b"h2"]).acceptor;
                        let accepting = tokio::spawn(async move {
                            let (socket, _) = listener.accept().await.unwrap();
                            acceptor.accept(socket).await.unwrap()
                        });
                        let client = tls_connector(if grpc { &["h2"] } else { &[] })
                            .connect(
                                "localhost",
                                tokio::net::TcpStream::connect(address).await.unwrap(),
                            )
                            .await
                            .unwrap();
                        (
                            volo::net::conn::Conn::new(
                                client,
                                volo::net::conn::ConnInfo { peer_addr: None },
                            ),
                            volo::net::conn::Conn::new(
                                accepting.await.unwrap(),
                                volo::net::conn::ConnInfo {
                                    peer_addr: Some(peer),
                                },
                            ),
                        )
                    }
                    #[cfg(not(feature = "rustls"))]
                    {
                        unreachable!()
                    }
                } else {
                    let client = tokio::net::TcpStream::connect(address).await.unwrap();
                    let (socket, _) = listener.accept().await.unwrap();
                    let mut conn = volo::net::conn::Conn::from(socket);
                    conn.info.peer_addr = Some(peer);
                    (client.into(), conn)
                };
                let server = tokio::spawn(prepared.run(OnceIncoming(Some(accepted))));
                if grpc {
                    let (mut client, connection) = h2::client::handshake(client).await.unwrap();
                    let driver = tokio::spawn(connection);
                    let payload: bytes::Bytes = pilota::pb::Message::encode_to_vec(
                        &pb::Event::default(),
                        &mut Default::default(),
                    )
                    .into();
                    let (response, _) = raw_request(
                        &mut client,
                        address,
                        "/streamx.presence.PresenceService/Get",
                        &["application/grpc"],
                        payload,
                        true,
                    );
                    assert_eq!(raw_response(response).await.1["grpc-status"], "0");
                    handle.shutdown();
                    drop(client);
                    server.await.unwrap().unwrap();
                    driver.await.unwrap().unwrap();
                } else {
                    client
                        .write_all(&ordinary_request(false, "hello", false))
                        .await
                        .unwrap();
                    use pilota::thrift::{Message as _, TAsyncInputProtocol};
                    let mut protocol = pilota::thrift::TAsyncBinaryProtocol::new(&mut client);
                    assert_eq!(
                        protocol.read_message_begin().await.unwrap().sequence_number,
                        131
                    );
                    ChatServiceGetResultRecv::decode_async(&mut protocol)
                        .await
                        .unwrap();
                    drop(client);
                    handle.shutdown();
                    server.await.unwrap().unwrap();
                }
                assert_eq!(handle.stats().accepted, 1);
                assert_eq!(handle.stats().rejected, 0);
                assert_eq!(handle.stats().active, 0);
            }
        }
    })
    .await
    .unwrap();
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn unified_custom_tls_alpn_is_checked_even_when_the_server_did_not_build_the_acceptor() {
    use tokio::io::AsyncReadExt;
    tokio::time::timeout(Duration::from_secs(5), async {
        let prepared = volo_streamx::Server::new()
            .add_service(chat_service_streamx::BoundService::from_arc(chat()))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let acceptor = tls_config(&[b"custom"]).acceptor;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            acceptor.accept(socket).await.unwrap()
        });
        let mut client = tls_connector(&["custom"])
            .connect(
                "localhost",
                tokio::net::TcpStream::connect(address).await.unwrap(),
            )
            .await
            .unwrap();
        let conn = volo::net::conn::Conn::new(
            accepted.await.unwrap(),
            volo::net::conn::ConnInfo { peer_addr: None },
        );
        let server = tokio::spawn(prepared.run(OnceIncoming(Some(conn))));
        let mut received = Vec::new();
        let _ = client.read_to_end(&mut received).await;
        assert!(received.is_empty());
        assert_eq!(handle.stats().rejected, 1);
        assert_eq!(handle.stats().detected, 0);
        handle.shutdown();
        server.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unified_observer_panic_keeps_connection_cleanup_and_business_available() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let shared = chat();
        let prepared = volo_streamx::Server::new()
            .add_service(chat_service_streamx::BoundService::from_arc(shared.clone()))
            .connection_observer(|_| panic!("observer failed"))
            .graceful_shutdown_timeout(Duration::from_millis(200))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let client = ChatServiceClientBuilder::new("chat")
            .address(address)
            .build();
        client.get(message(7)).await.unwrap();
        assert_eq!(shared.calls.load(Ordering::Relaxed), 1);
        handle.shutdown();
        task.await.unwrap().unwrap();
        assert_eq!(handle.stats().active, 0);
    })
    .await
    .unwrap();
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn unified_pending_tls_handshake_does_not_block_accept_or_shutdown() {
    use tokio::io::AsyncReadExt;
    tokio::time::timeout(Duration::from_secs(4), async {
        let prepared = volo_streamx::Server::new()
            .add_fallback_service(chat_service_streamx::BoundService::from_arc(chat()))
            .add_service(pb::presence_service_streamx::bind(Presence))
            .tls_config(tls_config(&[]))
            .graceful_shutdown_timeout(Duration::from_millis(200))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let mut idle = tokio::net::TcpStream::connect(address).await.unwrap();
        let connection = tls_connector(&["h2"])
            .connect(
                "localhost",
                tokio::net::TcpStream::connect(address).await.unwrap(),
            )
            .await
            .unwrap();
        let (mut client, connection) = h2::client::handshake(connection).await.unwrap();
        let connection = tokio::spawn(connection);
        let payload =
            pilota::pb::Message::encode_to_vec(&pb::Event::default(), &mut Default::default())
                .into();
        let (response, _) = raw_request(
            &mut client,
            address,
            "/streamx.presence.PresenceService/Get",
            &["application/grpc"],
            payload,
            true,
        );
        assert_eq!(raw_response(response).await.1["grpc-status"], "0");
        assert_eq!(handle.stats().accepted, 2);
        handle.shutdown();
        drop(client);
        server.await.unwrap().unwrap();
        connection.await.unwrap().unwrap();
        assert_eq!(idle.read(&mut [0; 1]).await.unwrap(), 0);
        assert_eq!(
            (
                handle.stats().active,
                handle.stats().active_calls,
                handle.stats().active_tasks
            ),
            (0, 0, 0)
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unified_global_layers_can_reject_every_protocol_before_polling_business() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let deny = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let shared = chat();
        let prepared = volo_streamx::Server::new()
            .add_fallback_service(chat_service_streamx::BoundService::from_arc(shared.clone()))
            .add_service(pb::presence_service_streamx::bind(Presence))
            .layer(AdmitLayer(deny.clone()))
            .graceful_shutdown_timeout(Duration::from_millis(500))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let native = ChatServiceClientBuilder::new("chat")
            .address(address)
            .build();
        assert!(native.get(message(7)).await.is_err());
        match native.watch(message(0)).await {
            Err(error) => assert!(
                matches!(
                    error,
                    volo_thrift::streaming::StreamError::Biz { code: 401, .. }
                ),
                "{error:?}"
            ),
            Ok(mut stream) => assert!(matches!(
                stream.recv().await,
                Err(volo_thrift::streaming::StreamError::Biz { code: 401, .. })
            )),
        }
        let grpc = streamx_codegen::grpc::chat::ChatServiceClientBuilder::new("chat")
            .address(address)
            .build();
        let error = grpc
            .get(streamx_codegen::grpc::chat::ChatServiceGetRequest {
                request: message(7),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), volo_grpc::Code::Internal);
        assert_eq!(error.metadata().get("biz-status").unwrap(), "401");
        let proto = pb::PresenceServiceClientBuilder::new("presence")
            .address(address)
            .build();
        let error = proto.get(pb::Event::default()).await.unwrap_err();
        assert_eq!(error.code(), volo_grpc::Code::Internal);
        assert_eq!(error.metadata().get("biz-status").unwrap(), "401");
        assert_eq!(shared.calls.load(Ordering::Relaxed), 0);
        deny.store(false, Ordering::Relaxed);
        native.get(message(7)).await.unwrap();
        let mut watch = native.watch(message(0)).await.unwrap();
        while watch.recv().await.unwrap().is_some() {}
        grpc.get(streamx_codegen::grpc::chat::ChatServiceGetRequest {
            request: message(7),
        })
        .await
        .unwrap();
        proto.get(pb::Event::default()).await.unwrap();
        assert_eq!(shared.calls.load(Ordering::Relaxed), 5);
        handle.shutdown();
        task.await.unwrap().unwrap();
        assert_eq!(
            (
                handle.stats().active,
                handle.stats().active_calls,
                handle.stats().active_tasks
            ),
            (0, 0, 0)
        );
    })
    .await
    .unwrap();
}

struct CancelFromHook {
    sent: Arc<AtomicUsize>,
    finished: Arc<AtomicUsize>,
    violations: Arc<AtomicUsize>,
}
impl volo_streamx::StreamHook for CancelFromHook {
    fn on_send(&self, cx: &StreamContext, _: usize) {
        self.sent.fetch_add(1, Ordering::Relaxed);
        cx.cancel(volo_streamx::RpcError::Canceled {
            cause: volo_streamx::CancelCause::Local,
            code: 0,
            message: "cancel from send observer".into(),
            path: Vec::new(),
        });
    }
    fn on_finish(&self, cx: &StreamContext, _: &volo_streamx::StreamStatus) {
        if !matches!(
            cx.cancellation(),
            Some(volo_streamx::RpcError::Canceled {
                cause: volo_streamx::CancelCause::Local,
                ..
            })
        ) {
            self.violations.fetch_add(1, Ordering::Relaxed);
        }
        self.finished.fetch_add(1, Ordering::Relaxed);
    }
}

#[tokio::test]
async fn unified_hook_can_cancel_from_send_without_reentrant_finish_or_deadlock() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let sent = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        let violations = Arc::new(AtomicUsize::new(0));
        let counts = (sent.clone(), finished.clone(), violations.clone());
        let prepared = volo_streamx::Server::new()
            .add_fallback_service(chat_service_streamx::BoundService::from_arc(chat()))
            .add_service(pb::presence_service_streamx::bind(Presence))
            .stream_hook(move |_: &StreamContext| CancelFromHook {
                sent: counts.0.clone(),
                finished: counts.1.clone(),
                violations: counts.2.clone(),
            })
            .graceful_shutdown_timeout(Duration::from_millis(500))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        match ChatServiceClientBuilder::new("chat")
            .address(address)
            .build()
            .watch(message(0))
            .await
        {
            Err(_) => {}
            Ok(mut stream) => loop {
                match stream.recv().await {
                    Ok(Some(_)) => {}
                    Err(_) => break,
                    Ok(None) => panic!("canceled TTStream must return an error"),
                }
            },
        }
        let grpc = streamx_codegen::grpc::chat::ChatServiceClientBuilder::new("chat")
            .address(address)
            .build();
        if let Ok(response) = grpc.watch(message(0)).await {
            let mut stream = response.into_inner();
            loop {
                match stream.message().await {
                    Ok(Some(_)) => {}
                    Err(_) => break,
                    Ok(None) => panic!("canceled Thrift gRPC must return an error"),
                }
            }
        }
        let proto = pb::PresenceServiceClientBuilder::new("presence")
            .address(address)
            .build();
        if let Ok(response) = proto.watch(pb::Event::default()).await {
            let mut stream = Box::pin(response.into_inner());
            loop {
                match stream.next().await {
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                    None => panic!("canceled Protobuf gRPC must return an error"),
                }
            }
        }
        handle.shutdown();
        server.await.unwrap().unwrap();
        assert_eq!(sent.load(Ordering::Relaxed), 3);
        assert_eq!(finished.load(Ordering::Relaxed), 3);
        assert_eq!(violations.load(Ordering::Relaxed), 0);
        assert_eq!(
            (
                handle.stats().active,
                handle.stats().active_calls,
                handle.stats().active_tasks
            ),
            (0, 0, 0)
        );
    })
    .await
    .unwrap();
}

#[derive(Clone, Copy, Debug)]
enum HookFault {
    Disabled,
    EnabledPanic,
    CreatePanic,
    OpenPanic,
    OpenError,
    SendPanic,
    FinishPanic,
}
struct FaultFactory {
    fault: HookFault,
    creates: Arc<AtomicUsize>,
    finishes: Arc<AtomicUsize>,
}
struct FaultObserver {
    fault: HookFault,
    finishes: Arc<AtomicUsize>,
}
impl volo_streamx::StreamHookFactory for FaultFactory {
    fn enabled(&self, _: &StreamContext) -> bool {
        assert!(
            !matches!(self.fault, HookFault::EnabledPanic),
            "enabled panic"
        );
        !matches!(self.fault, HookFault::Disabled)
    }
    fn create(&self, _: &StreamContext) -> Arc<dyn volo_streamx::StreamHook> {
        self.creates.fetch_add(1, Ordering::Relaxed);
        assert!(
            !matches!(self.fault, HookFault::CreatePanic),
            "factory panic"
        );
        Arc::new(FaultObserver {
            fault: self.fault,
            finishes: self.finishes.clone(),
        })
    }
}
impl volo_streamx::StreamHook for FaultObserver {
    fn on_open(&self, _: &StreamContext) -> RpcResult<()> {
        assert!(!matches!(self.fault, HookFault::OpenPanic), "open panic");
        if matches!(self.fault, HookFault::OpenError) {
            return Err(volo_streamx::RpcError::Biz {
                code: 4701,
                message: "hook admission denied".into(),
                extra: Default::default(),
            });
        }
        Ok(())
    }
    fn on_send(&self, _: &StreamContext, _: usize) {
        assert!(
            !matches!(self.fault, HookFault::SendPanic),
            "send observer panic"
        );
    }
    fn on_finish(&self, _: &StreamContext, _: &volo_streamx::StreamStatus) {
        self.finishes.fetch_add(1, Ordering::Relaxed);
        assert!(
            !matches!(self.fault, HookFault::FinishPanic),
            "finish panic"
        );
    }
}
struct CountingPresence(Arc<AtomicUsize>);
impl pb::presence_service_streamx::Handler for CountingPresence {
    async fn get(&self, _: StreamContext, request: pb::Event) -> RpcResult<pb::Event> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(request)
    }
    async fn upload(&self, _: StreamContext, _: RequestStream<pb::Event>) -> RpcResult<pb::Event> {
        unreachable!()
    }
    async fn watch(
        &self,
        _: StreamContext,
        _: pb::Event,
        _: ResponseSender<pb::Event>,
    ) -> RpcResult<()> {
        unreachable!()
    }
    async fn talk(
        &self,
        _: StreamContext,
        _: RequestStream<pb::Event>,
        _: ResponseSender<pb::Event>,
    ) -> RpcResult<()> {
        unreachable!()
    }
}

#[tokio::test]
async fn unified_hook_admission_and_panics_keep_other_observers_balanced() {
    tokio::time::timeout(Duration::from_secs(8), async {
        for fault in [
            HookFault::Disabled,
            HookFault::EnabledPanic,
            HookFault::CreatePanic,
            HookFault::OpenPanic,
            HookFault::OpenError,
            HookFault::SendPanic,
            HookFault::FinishPanic,
        ] {
            let creates = Arc::new(AtomicUsize::new(0));
            let finishes = Arc::new(AtomicUsize::new(0));
            let healthy = Arc::new(SharedHookCounts::default());
            let observed = healthy.clone();
            let shared = chat();
            let proto_calls = Arc::new(AtomicUsize::new(0));
            let prepared = volo_streamx::Server::new()
                .add_fallback_service(chat_service_streamx::BoundService::from_arc(shared.clone()))
                .add_service(pb::presence_service_streamx::bind(CountingPresence(
                    proto_calls.clone(),
                )))
                .stream_hook(FaultFactory {
                    fault,
                    creates: creates.clone(),
                    finishes,
                })
                .stream_hook(move |cx: &StreamContext| shared_observer(&observed, cx))
                .graceful_shutdown_timeout(Duration::from_millis(500))
                .prepare()
                .unwrap();
            let handle = prepared.handle();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
            let admitted = !matches!(
                fault,
                HookFault::EnabledPanic
                    | HookFault::CreatePanic
                    | HookFault::OpenPanic
                    | HookFault::OpenError
            );
            let native = ChatServiceClientBuilder::new("chat")
                .address(address)
                .build();
            assert_eq!(native.get(message(7)).await.is_ok(), admitted, "{fault:?}");
            let streaming_ok = match native.watch(message(0)).await {
                Err(_) => false,
                Ok(mut stream) => {
                    let mut messages = 0;
                    loop {
                        match stream.recv().await {
                            Ok(Some(_)) => messages += 1,
                            Ok(None) => break messages == 3,
                            Err(_) => break false,
                        }
                    }
                }
            };
            assert_eq!(streaming_ok, admitted, "{fault:?}");
            let grpc = streamx_codegen::grpc::chat::ChatServiceClientBuilder::new("chat")
                .address(address)
                .build();
            let result = grpc
                .get(streamx_codegen::grpc::chat::ChatServiceGetRequest {
                    request: message(7),
                })
                .await;
            assert_eq!(result.is_ok(), admitted, "{fault:?}");
            if matches!(fault, HookFault::OpenError) {
                assert_eq!(
                    result.unwrap_err().metadata().get("biz-status").unwrap(),
                    "4701"
                );
            }
            let proto = pb::PresenceServiceClientBuilder::new("presence")
                .address(address)
                .build();
            let result = proto.get(pb::Event::default()).await;
            assert_eq!(result.is_ok(), admitted, "{fault:?}");
            if matches!(fault, HookFault::OpenError) {
                assert_eq!(
                    result.unwrap_err().metadata().get("biz-status").unwrap(),
                    "4701"
                );
            }
            handle.shutdown();
            server.await.unwrap().unwrap();
            assert_eq!(
                shared.calls.load(Ordering::Relaxed),
                if admitted { 5 } else { 0 },
                "{fault:?}"
            );
            assert_eq!(
                proto_calls.load(Ordering::Relaxed),
                usize::from(admitted),
                "{fault:?}"
            );
            assert_eq!(
                creates.load(Ordering::Relaxed),
                if matches!(fault, HookFault::Disabled | HookFault::EnabledPanic) {
                    0
                } else {
                    4
                }
            );
            let opened = if matches!(fault, HookFault::EnabledPanic | HookFault::CreatePanic) {
                0
            } else {
                4
            };
            assert_eq!(healthy.opened.load(Ordering::Relaxed), opened, "{fault:?}");
            assert_eq!(
                healthy.finished.load(Ordering::Relaxed),
                opened,
                "{fault:?}"
            );
            assert_eq!(healthy.violations.load(Ordering::Relaxed), 0, "{fault:?}");
            assert_eq!(
                (handle.stats().active_calls, handle.stats().active_tasks),
                (0, 0)
            );
        }
    })
    .await
    .unwrap();
}

#[derive(Default)]
struct FloodState {
    contexts: std::sync::Mutex<Vec<StreamContext>>,
    changed: tokio::sync::Notify,
    reclaimed: Arc<AtomicUsize>,
}
impl FloodState {
    async fn wait(&self, count: usize) -> StreamContext {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.contexts.lock().unwrap().len() >= count {
                return self.contexts.lock().unwrap()[count - 1].clone();
            }
            changed.await;
        }
    }
    async fn run<T: Send + 'static>(
        self: &Arc<Self>,
        cx: StreamContext,
        value: T,
        mut output: ResponseSender<T>,
        empty: bool,
    ) -> RpcResult<()> {
        check_framework_scope(&cx);
        let _handler = DropCount(self.reclaimed.clone());
        if !empty {
            let started = Arc::new(tokio::sync::Notify::new());
            let ready = started.clone();
            let reclaimed = self.reclaimed.clone();
            let child_cx = cx.clone();
            drop(cx.spawn_scoped(async move {
                check_framework_scope(&child_cx);
                let _child = DropCount(reclaimed);
                ready.notify_one();
                futures::future::pending::<()>().await;
            }));
            started.notified().await;
        }
        self.contexts.lock().unwrap().push(cx);
        self.changed.notify_waiters();
        if empty {
            return Ok(());
        }
        for _ in 0..4096 {
            output.send(&value).await?;
        }
        Ok(())
    }
}
struct FloodChat(Arc<FloodState>);
impl chat_service_streamx::Handler for FloodChat {
    async fn get(
        &self,
        _: StreamContext,
        value: Message,
    ) -> RpcResult<Result<Message, ChatServiceGetException>> {
        Ok(Ok(value))
    }
    async fn watch(
        &self,
        cx: StreamContext,
        value: Message,
        output: ResponseSender<Message>,
    ) -> RpcResult<()> {
        let empty = value.text == "empty";
        self.0.run(cx, value, output, empty).await
    }
    async fn ping(&self, _: StreamContext, _: ()) -> RpcResult<()> {
        Ok(())
    }
    async fn fire(&self, _: StreamContext, _: Message) -> RpcResult<()> {
        Ok(())
    }
    async fn add(&self, _: StreamContext, _: ChatServiceAddArgsRecv) -> RpcResult<i64> {
        unreachable!()
    }
    async fn upload(&self, _: StreamContext, _: RequestStream<Message>) -> RpcResult<Message> {
        unreachable!()
    }
    async fn talk(
        &self,
        _: StreamContext,
        _: RequestStream<Message>,
        _: ResponseSender<Message>,
    ) -> RpcResult<()> {
        unreachable!()
    }
}
struct FloodPresence(Arc<FloodState>);
impl pb::presence_service_streamx::Handler for FloodPresence {
    async fn get(&self, _: StreamContext, value: pb::Event) -> RpcResult<pb::Event> {
        Ok(value)
    }
    async fn watch(
        &self,
        cx: StreamContext,
        value: pb::Event,
        output: ResponseSender<pb::Event>,
    ) -> RpcResult<()> {
        let empty = value.text == "empty";
        self.0.run(cx, value, output, empty).await
    }
    async fn upload(&self, _: StreamContext, _: RequestStream<pb::Event>) -> RpcResult<pb::Event> {
        unreachable!()
    }
    async fn talk(
        &self,
        _: StreamContext,
        _: RequestStream<pb::Event>,
        _: ResponseSender<pb::Event>,
    ) -> RpcResult<()> {
        unreachable!()
    }
}
#[derive(Clone)]
struct DropResponseLayer(Arc<std::sync::atomic::AtomicBool>);
#[derive(Clone)]
struct DropResponseService<S> {
    inner: S,
    dropping: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Default)]
struct BindingCounts {
    opened: AtomicUsize,
    finished: AtomicUsize,
    layers: AtomicUsize,
    violations: AtomicUsize,
}
struct BindingHook {
    service: &'static str,
    counts: Arc<BindingCounts>,
}
impl volo_grpc::streaming::StreamHook for BindingHook {
    fn on_open(&self, cx: &volo_grpc::streaming::StreamContext) -> Result<(), volo_grpc::Status> {
        self.counts.opened.fetch_add(1, Ordering::Relaxed);
        if cx.shared_context().service() != self.service {
            self.counts.violations.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
    fn on_finish(
        &self,
        _: &volo_grpc::streaming::StreamContext,
        status: &volo_grpc::streaming::StreamStatus,
    ) {
        self.counts.finished.fetch_add(1, Ordering::Relaxed);
        if status.status.code() != volo_grpc::Code::Ok {
            self.counts.violations.fetch_add(1, Ordering::Relaxed);
        }
    }
}
#[derive(Clone)]
struct BindingLayer {
    service: &'static str,
    counts: Arc<BindingCounts>,
}
#[derive(Clone)]
struct BindingService<S> {
    inner: S,
    layer: BindingLayer,
}
impl<S> volo::Layer<S> for BindingLayer {
    type Service = BindingService<S>;
    fn layer(self, inner: S) -> Self::Service {
        BindingService { inner, layer: self }
    }
}
impl<S: volo_grpc::server::NamedService> volo_grpc::server::NamedService for BindingService<S> {
    const NAME: &'static str = S::NAME;
    fn descriptor() -> Option<volo_grpc::message::ServiceDescriptor> {
        S::descriptor()
    }
    fn requires_managed_transport() -> bool {
        S::requires_managed_transport()
    }
}
impl<S, T, U> volo::Service<volo_grpc::context::ServerContext, volo_grpc::Request<T>>
    for BindingService<S>
where
    S: volo::Service<
            volo_grpc::context::ServerContext,
            volo_grpc::Request<T>,
            Response = volo_grpc::Response<U>,
        > + Send
        + Sync,
    S::Error: Into<volo_grpc::Status>,
    T: Send,
{
    type Response = volo_grpc::Response<U>;
    type Error = volo_grpc::Status;
    async fn call(
        &self,
        cx: &mut volo_grpc::context::ServerContext,
        request: volo_grpc::Request<T>,
    ) -> Result<Self::Response, Self::Error> {
        self.layer.counts.layers.fetch_add(1, Ordering::Relaxed);
        let mut response = self.inner.call(cx, request).await.map_err(Into::into)?;
        response
            .metadata_mut()
            .insert("x-binding-service", self.layer.service.parse().unwrap());
        Ok(response)
    }
}

fn gzip_frame(payload: &[u8]) -> bytes::Bytes {
    use std::io::Write;
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(payload).unwrap();
    let compressed = gzip.finish().unwrap();
    let mut frame = vec![1];
    frame.extend_from_slice(&u32::try_from(compressed.len()).unwrap().to_be_bytes());
    frame.extend_from_slice(&compressed);
    frame.into()
}
fn inflate_frames(mut body: &[u8]) -> Vec<bytes::Bytes> {
    use std::io::Read;
    let mut messages = Vec::new();
    while !body.is_empty() {
        assert!(body.len() >= 5);
        assert_eq!(body[0], 1, "response DATA must use gzip");
        let length = u32::from_be_bytes(body[1..5].try_into().unwrap()) as usize;
        assert!(body.len() >= 5 + length);
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(&body[5..5 + length])
            .read_to_end(&mut decoded)
            .unwrap();
        messages.push(decoded.into());
        body = &body[5 + length..];
    }
    messages
}
fn encoded_request(
    client: &mut h2::client::SendRequest<bytes::Bytes>,
    address: std::net::SocketAddr,
    path: &str,
    content_type: &str,
    body: bytes::Bytes,
    gzip: bool,
) -> h2::client::ResponseFuture {
    let mut request = http::Request::builder()
        .method("POST")
        .uri(format!("http://{address}{path}"))
        .header("content-type", content_type)
        .header("te", "trailers")
        .header("grpc-accept-encoding", "gzip")
        .header("x-request", "metadata-value")
        .body(())
        .unwrap();
    if gzip {
        request
            .headers_mut()
            .insert("grpc-encoding", "gzip".parse().unwrap());
    }
    let (response, mut upload) = client.send_request(request, false).unwrap();
    upload.send_data(body, true).unwrap();
    response
}

#[tokio::test]
async fn unified_binding_configuration_preserves_gzip_layers_and_native_hooks_for_both_idls() {
    use volo_grpc::codec::compression::CompressionEncoding;
    tokio::time::timeout(Duration::from_secs(8), async {
        let shared = chat();
        let thrift_counts = Arc::new(BindingCounts::default());
        let proto_counts = Arc::new(BindingCounts::default());
        let thrift_hooks = thrift_counts.clone();
        let proto_hooks = proto_counts.clone();
        let bound = chat_service_streamx::BoundService::from_arc(shared.clone()).configure_grpc(
            |service| {
                service
                    .send_compressions(vec![CompressionEncoding::Gzip(None)])
                    .accept_compressions(vec![CompressionEncoding::Gzip(None)])
                    .layer(BindingLayer {
                        service: "ChatService",
                        counts: thrift_counts.clone(),
                    })
                    .stream_hook(move |_: &volo_grpc::streaming::StreamContext| BindingHook {
                        service: "ChatService",
                        counts: thrift_hooks.clone(),
                    })
            },
        );
        let presence = pb::presence_service_streamx::bind(Presence).configure_grpc(|service| {
            service
                .send_compressions(vec![CompressionEncoding::Gzip(None)])
                .accept_compressions(vec![CompressionEncoding::Gzip(None)])
                .layer(BindingLayer {
                    service: "PresenceService",
                    counts: proto_counts.clone(),
                })
                .stream_hook(move |_: &volo_grpc::streaming::StreamContext| BindingHook {
                    service: "PresenceService",
                    counts: proto_hooks.clone(),
                })
        });
        assert!(Arc::ptr_eq(bound.handler(), &shared));
        let common = Arc::new(SharedHookCounts::default());
        let observer = common.clone();
        let prepared = volo_streamx::Server::new()
            .add_fallback_service(bound.clone())
            .add_service(presence.clone())
            .stream_hook(move |cx: &StreamContext| shared_observer(&observer, cx))
            .graceful_shutdown_timeout(Duration::from_secs(1))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut client, driver) = h2::client::handshake(socket).await.unwrap();
        let driver = tokio::spawn(driver);
        for thrift in [true, false] {
            for method in ["Get", "Upload", "Watch", "Talk"] {
                let streaming_input = matches!(method, "Upload" | "Talk");
                let mut request = message(7);
                request.text = "gzip".repeat(128).into();
                let event = pb::Event {
                    text: request.text.clone(),
                    sequence: 7,
                };
                let payload = if thrift && method == "Get" {
                    thrift_bytes(&ChatServiceGetArgsSend {
                        request: request.clone(),
                    })
                } else if thrift {
                    thrift_bytes(&request)
                } else {
                    pilota::pb::Message::encode_to_vec(&event, &mut Default::default()).into()
                };
                let mut body = gzip_frame(&payload).to_vec();
                if streaming_input {
                    body.extend_from_slice(&gzip_frame(&payload));
                }
                let service = if thrift {
                    "ChatService"
                } else {
                    "streamx.presence.PresenceService"
                };
                let content_type = if thrift {
                    "application/grpc+thrift"
                } else {
                    "application/grpc"
                };
                let response = encoded_request(
                    &mut client,
                    address,
                    &format!("/{service}/{method}"),
                    content_type,
                    body.into(),
                    true,
                );
                let (status, headers, body) = raw_response(response).await;
                assert_eq!(status, http::StatusCode::OK);
                assert_eq!(
                    headers["grpc-status"], "0",
                    "{service}/{method}: {headers:?}"
                );
                assert_eq!(headers["grpc-encoding"], "gzip");
                assert_eq!(
                    headers["x-binding-service"],
                    service.rsplit('.').next().unwrap()
                );
                let messages = inflate_frames(&body);
                let count = if method == "Talk" {
                    2
                } else if thrift && method == "Watch" {
                    3
                } else {
                    1
                };
                assert_eq!(messages.len(), count);
                for (index, message) in messages.iter().enumerate() {
                    if thrift {
                        let mut expected = request.clone();
                        if method == "Upload" {
                            expected.text = "upload@shared".into();
                            expected.sequence = 2;
                        } else {
                            expected.text = format!("{}@shared", request.text).into();
                        }
                        if method == "Watch" {
                            expected.sequence = index as i32;
                        }
                        let expected = if method == "Get" {
                            thrift_bytes(&ChatServiceGetResultSend::Ok(expected))
                        } else {
                            thrift_bytes(&expected)
                        };
                        assert_eq!(*message, expected);
                    } else {
                        let expected = if method == "Upload" {
                            pb::Event {
                                text: "".into(),
                                sequence: 14,
                            }
                        } else {
                            event.clone()
                        };
                        assert_eq!(
                            *message,
                            pilota::pb::Message::encode_to_vec(&expected, &mut Default::default())
                        );
                    }
                }
            }
        }
        let native = ChatServiceClientBuilder::new("chat")
            .address(address)
            .build();
        native.get(message(7)).await.unwrap();
        let mut stream = native.watch(message(7)).await.unwrap();
        while stream.recv().await.unwrap().is_some() {}
        handle.shutdown();
        drop(client);
        server.await.unwrap().unwrap();
        driver.await.unwrap().unwrap();
        for counts in [thrift_counts, proto_counts] {
            assert_eq!(counts.opened.load(Ordering::Relaxed), 4);
            assert_eq!(counts.finished.load(Ordering::Relaxed), 4);
            assert_eq!(counts.layers.load(Ordering::Relaxed), 4);
            assert_eq!(counts.violations.load(Ordering::Relaxed), 0);
        }
        assert_eq!(common.opened.load(Ordering::Relaxed), 10);
        assert_eq!(common.finished.load(Ordering::Relaxed), 10);
        assert_eq!(common.violations.load(Ordering::Relaxed), 0);
        assert_eq!(handle.stats().active_calls, 0);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unified_binding_limits_are_per_service_and_recover_on_the_same_h2_connection() {
    use volo_grpc::codec::{CodecLimits, compression::CompressionEncoding};
    tokio::time::timeout(Duration::from_secs(10), async {
        for thrift in [true, false] {
            for budget in ["send", "receive", "decompressed"] {
                let mut limits = CodecLimits::default();
                match budget {
                    "send" => limits.max_send_message_size = 128,
                    "receive" => limits.max_receive_message_size = 128,
                    _ => limits.max_decompressed_message_size = 128,
                }
                let shared = chat();
                let bound = chat_service_streamx::BoundService::from_arc(shared.clone());
                let bound = if thrift {
                    bound.configure_grpc(|service| {
                        service
                            .codec_limits(limits)
                            .accept_compressions(vec![CompressionEncoding::Gzip(None)])
                    })
                } else {
                    bound
                };
                let presence = pb::presence_service_streamx::bind(Presence);
                let presence = if !thrift {
                    presence.configure_grpc(|service| {
                        service
                            .codec_limits(limits)
                            .accept_compressions(vec![CompressionEncoding::Gzip(None)])
                    })
                } else {
                    presence
                };
                let prepared = volo_streamx::Server::new()
                    .add_fallback_service(bound)
                    .add_service(presence)
                    .graceful_shutdown_timeout(Duration::from_secs(1))
                    .prepare()
                    .unwrap();
                let handle = prepared.handle();
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
                let socket = tokio::net::TcpStream::connect(address).await.unwrap();
                let (mut client, driver) = h2::client::handshake(socket).await.unwrap();
                let driver = tokio::spawn(driver);
                for (payload_is_thrift, large) in [(thrift, true), (!thrift, true), (thrift, false)]
                {
                    let mut request = message(7);
                    if large {
                        request.text = "x".repeat(512).into();
                    }
                    let payload = if payload_is_thrift {
                        thrift_bytes(&ChatServiceGetArgsSend { request })
                    } else {
                        pilota::pb::Message::encode_to_vec(
                            &pb::Event {
                                text: request.text,
                                sequence: 7,
                            },
                            &mut Default::default(),
                        )
                        .into()
                    };
                    let compressed = budget == "decompressed" && payload_is_thrift == thrift;
                    let body = if compressed {
                        gzip_frame(&payload)
                    } else {
                        grpc_frame(payload)
                    };
                    let (path, content_type) = if payload_is_thrift {
                        ("/ChatService/Get", "application/grpc+thrift")
                    } else {
                        ("/streamx.presence.PresenceService/Get", "application/grpc")
                    };
                    let response =
                        encoded_request(&mut client, address, path, content_type, body, compressed);
                    let (_, headers, _) = raw_response(response).await;
                    let code = if payload_is_thrift == thrift && large {
                        "8"
                    } else {
                        "0"
                    };
                    assert_eq!(
                        headers["grpc-status"], code,
                        "{payload_is_thrift}/{budget}/{large}: {headers:?}"
                    );
                }
                assert_eq!(handle.stats().accepted, 1);
                let calls = if thrift && budget == "send" { 2 } else { 1 };
                assert_eq!(shared.calls.load(Ordering::Relaxed), calls);
                handle.shutdown();
                drop(client);
                server.await.unwrap().unwrap();
                driver.await.unwrap().unwrap();
                assert_eq!(handle.stats().active_calls, 0);
            }
        }
    })
    .await
    .unwrap();
}

#[derive(Default)]
struct MetadataCounts {
    finished: AtomicUsize,
    metas: AtomicUsize,
    headers: AtomicUsize,
    trailers: AtomicUsize,
    half_send: AtomicUsize,
    half_recv: AtomicUsize,
    violations: AtomicUsize,
}
struct MetadataHook(Arc<MetadataCounts>);
impl MetadataHook {
    fn check(&self, valid: bool) {
        if !valid {
            self.0.violations.fetch_add(1, Ordering::Relaxed);
        }
    }
}
impl volo_streamx::StreamHook for MetadataHook {
    fn on_open(&self, cx: &StreamContext) -> RpcResult<()> {
        self.check(
            cx.metadata()
                .get("x-request")
                .is_some_and(|value| value.as_ref() == b"metadata-value"),
        );
        let mut header = volo_streamx::Metadata::default();
        header.insert("x-out-header", "header-value");
        cx.response_headers(header)?;
        let mut trailer = volo_streamx::Metadata::default();
        trailer.insert("x-out-trailer", "trailer-value");
        cx.response_trailers(trailer)
    }
    fn on_header(
        &self,
        _: &StreamContext,
        direction: volo_streamx::StreamDirection,
        metadata: &volo_streamx::Metadata,
    ) {
        if direction == volo_streamx::StreamDirection::Send {
            self.check(
                metadata
                    .get("x-out-header")
                    .is_some_and(|value| value.as_ref() == b"header-value"),
            );
            self.0.headers.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn on_meta(
        &self,
        cx: &StreamContext,
        direction: volo_streamx::StreamDirection,
        metadata: &volo_streamx::Metadata,
    ) {
        self.check(direction == volo_streamx::StreamDirection::Recv);
        self.check(
            metadata
                .get("x-meta")
                .is_some_and(|value| value.as_ref() == [0, 255]),
        );
        self.check(cx.metadata().get("x-meta") == metadata.get("x-meta"));
        self.0.metas.fetch_add(1, Ordering::Relaxed);
    }
    fn on_trailer(
        &self,
        _: &StreamContext,
        direction: volo_streamx::StreamDirection,
        metadata: &volo_streamx::Metadata,
    ) {
        if direction == volo_streamx::StreamDirection::Send {
            self.check(
                metadata
                    .get("x-out-trailer")
                    .is_some_and(|value| value.as_ref() == b"trailer-value"),
            );
            self.0.trailers.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn on_half_close(&self, cx: &StreamContext, direction: volo_streamx::StreamDirection) {
        self.check(StreamContext::current().is_some_and(|current| current.id() == cx.id()));
        let count = match direction {
            volo_streamx::StreamDirection::Send => &self.0.half_send,
            volo_streamx::StreamDirection::Recv => &self.0.half_recv,
        };
        count.fetch_add(1, Ordering::Relaxed);
    }
    fn on_finish(&self, _: &StreamContext, status: &volo_streamx::StreamStatus) {
        self.check(status.error.is_none());
        self.check(
            status
                .header
                .get("x-out-header")
                .is_some_and(|value| value.as_ref() == b"header-value"),
        );
        self.check(
            status
                .trailer
                .get("x-out-trailer")
                .is_some_and(|value| value.as_ref() == b"trailer-value"),
        );
        self.0.finished.fetch_add(1, Ordering::Relaxed);
    }
}

#[tokio::test]
async fn unified_common_metadata_matches_wire_and_native_terminal_snapshots() {
    use tokio::io::AsyncWriteExt;
    use volo_thrift::transport::ttstream::frame::{Frame, FrameType, read_frame};
    tokio::time::timeout(Duration::from_secs(6), async {
        let counts = Arc::new(MetadataCounts::default());
        let hooks = counts.clone();
        let prepared = volo_streamx::Server::new()
            .add_fallback_service(chat_service_streamx::BoundService::from_arc(chat()))
            .add_service(pb::presence_service_streamx::bind(Presence))
            .stream_hook(move |_: &StreamContext| MetadataHook(hooks.clone()))
            .graceful_shutdown_timeout(Duration::from_secs(1))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let mut ttstream = tokio::net::TcpStream::connect(address).await.unwrap();
        let config = volo_thrift::streaming::StreamConfig::default();
        for (kind, payload) in [
            (FrameType::Header, bytes::Bytes::new()),
            (FrameType::Meta, bytes::Bytes::new()),
            (FrameType::Data, thrift_bytes(&message(7))),
            (FrameType::Trailer, bytes::Bytes::new()),
        ] {
            let mut frame = Frame::new(-17, kind, bytes::Bytes::from_static(b"Talk"), payload);
            if kind == FrameType::Header {
                frame.metadata.insert("x-request", "metadata-value");
            }
            if kind == FrameType::Meta {
                frame
                    .metadata
                    .insert("x-meta", bytes::Bytes::from_static(&[0, 255]));
            }
            ttstream
                .write_all(&frame.encode(&config).unwrap())
                .await
                .unwrap();
        }
        let mut kinds = Vec::new();
        loop {
            let frame = read_frame(&mut ttstream, &config).await.unwrap().unwrap();
            assert_eq!(frame.stream_id, -17);
            match frame.kind {
                FrameType::Header => assert_eq!(
                    frame.metadata.get("x-out-header").map(bytes::Bytes::as_ref),
                    Some(&b"header-value"[..])
                ),
                FrameType::Data => assert!(!frame.payload.is_empty()),
                FrameType::Trailer => assert_eq!(
                    frame
                        .metadata
                        .get("x-out-trailer")
                        .map(bytes::Bytes::as_ref),
                    Some(&b"trailer-value"[..])
                ),
                other => panic!("unexpected frame: {other:?}"),
            }
            kinds.push(frame.kind);
            if frame.kind == FrameType::Trailer {
                break;
            }
        }
        assert_eq!(
            kinds,
            [FrameType::Header, FrameType::Data, FrameType::Trailer]
        );
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut client, driver) = h2::client::handshake(socket).await.unwrap();
        let driver = tokio::spawn(driver);
        for thrift in [true, false] {
            let (path, content_type, payload) = if thrift {
                (
                    "/ChatService/Talk",
                    "application/grpc+thrift",
                    thrift_bytes(&message(7)),
                )
            } else {
                (
                    "/streamx.presence.PresenceService/Talk",
                    "application/grpc",
                    pilota::pb::Message::encode_to_vec(
                        &pb::Event {
                            text: "hello".into(),
                            sequence: 7,
                        },
                        &mut Default::default(),
                    )
                    .into(),
                )
            };
            let response = encoded_request(
                &mut client,
                address,
                path,
                content_type,
                grpc_frame(payload),
                false,
            );
            let (_, headers, body) = raw_response(response).await;
            assert_eq!(headers["grpc-status"], "0");
            assert_eq!(headers["x-out-header"], "header-value");
            assert_eq!(headers["x-out-trailer"], "trailer-value");
            assert!(!body.is_empty());
        }
        handle.shutdown();
        drop((client, ttstream));
        server.await.unwrap().unwrap();
        driver.await.unwrap().unwrap();
        assert_eq!(counts.finished.load(Ordering::Relaxed), 3);
        assert_eq!(counts.metas.load(Ordering::Relaxed), 1);
        assert_eq!(counts.headers.load(Ordering::Relaxed), 3);
        assert_eq!(counts.trailers.load(Ordering::Relaxed), 3);
        // Every Talk handler explicitly drains its input to EOF before returning.
        assert_eq!(counts.half_send.load(Ordering::Relaxed), 3);
        assert_eq!(counts.half_recv.load(Ordering::Relaxed), 3);
        assert_eq!(counts.violations.load(Ordering::Relaxed), 0);
        assert_eq!(handle.stats().active_calls, 0);
    })
    .await
    .unwrap();
}
impl<S> volo::Layer<S> for DropResponseLayer {
    type Service = DropResponseService<S>;
    fn layer(self, inner: S) -> Self::Service {
        DropResponseService {
            inner,
            dropping: self.0,
        }
    }
}
impl<S>
    volo::Service<volo_grpc::context::ServerContext, volo_grpc::Request<volo_grpc::body::BoxBody>>
    for DropResponseService<S>
where
    S: volo::Service<
            volo_grpc::context::ServerContext,
            volo_grpc::Request<volo_grpc::body::BoxBody>,
            Response = volo_grpc::Response<volo_grpc::body::BoxBody>,
        > + Send
        + Sync,
    S::Error: Into<volo_grpc::Status>,
{
    type Response = volo_grpc::Response<volo_grpc::body::BoxBody>;
    type Error = volo_grpc::Status;
    async fn call(
        &self,
        cx: &mut volo_grpc::context::ServerContext,
        request: volo_grpc::Request<volo_grpc::body::BoxBody>,
    ) -> Result<Self::Response, Self::Error> {
        let response = self.inner.call(cx, request).await.map_err(Into::into)?;
        if self.dropping.load(Ordering::Relaxed) {
            drop(response);
            return Err(volo_grpc::Status::cancelled("test dropped response body"));
        }
        Ok(response)
    }
}

#[tokio::test]
async fn unified_hooks_zero_window_reset_disconnect_and_body_drop_reclaim_owned_work() {
    tokio::time::timeout(Duration::from_secs(12), async {
        for thrift in [false, true] {
            for action in ["reset", "disconnect", "body-drop"] {
                let state = Arc::new(FloodState::default());
                let hooks = Arc::new(SharedHookCounts::default()); let observed = hooks.clone();
                let dropping = Arc::new(std::sync::atomic::AtomicBool::new(action == "body-drop"));
                let prepared = volo_streamx::Server::new()
                    .add_fallback_service(chat_service_streamx::bind(FloodChat(state.clone())))
                    .add_service(pb::presence_service_streamx::bind(FloodPresence(state.clone())))
                    .stream_hook(move |cx: &StreamContext| shared_observer(&observed, cx))
                    .grpc(|server| server.layer(DropResponseLayer(dropping.clone())))
                    .graceful_shutdown_timeout(Duration::from_millis(500)).prepare().unwrap();
                let handle = prepared.handle();
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(); let address = listener.local_addr().unwrap();
                let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
                let (mut client, connection) = h2::client::Builder::new().initial_window_size(0).handshake(tokio::net::TcpStream::connect(address).await.unwrap()).await.unwrap();
                let mut driver = Some(tokio::spawn(connection));
                let path = if thrift { "/ChatService/Watch" } else { "/streamx.presence.PresenceService/Watch" };
                let types = if thrift { "application/grpc+thrift" } else { "application/grpc" };
                let payload = if thrift { thrift_bytes(&Message { text: "x".repeat(24 * 1024).into(), sequence: 0 }) } else { pilota::pb::Message::encode_to_vec(&pb::Event { text: "x".repeat(24 * 1024).into(), sequence: 0 }, &mut Default::default()).into() };
                let (response, mut upload) = raw_request(&mut client, address, path, &[types], payload, true);
                let cx = state.wait(1).await;
                let response = response.await.unwrap();
                if action == "body-drop" {
                    assert_eq!(response.headers()["grpc-status"], "1");
                } else {
                    assert!(cx.status().is_none(), "zero window must leave the flood in progress");
                    if action == "reset" { upload.send_reset(h2::Reason::CANCEL); }
                    else { let task = driver.take().unwrap(); task.abort(); let _ = task.await; }
                }
                let error = cx.cancelled().await;
                if action == "disconnect" { assert!(matches!(error, volo_streamx::RpcError::Transport { .. }), "{error:?}"); }
                else { assert!(matches!(&error, volo_streamx::RpcError::GrpcStatus(status) if status.code == 1), "{error:?}"); }
                cx.reclaimed().await;
                assert_eq!(state.reclaimed.load(Ordering::Acquire), 2, "{thrift} {action}");
                drop(response); drop(upload);
                dropping.store(false, Ordering::Relaxed);
                if action != "disconnect" {
                    let (path, types, payload) = if thrift { ("/streamx.presence.PresenceService/Watch", "application/grpc", pilota::pb::Message::encode_to_vec(&pb::Event { text: "empty".into(), sequence: 0 }, &mut Default::default()).into()) } else { ("/ChatService/Watch", "application/grpc+thrift", thrift_bytes(&Message { text: "empty".into(), sequence: 0 })) };
                    let (response, _) = raw_request(&mut client, address, path, &[types], payload, true);
                    assert_eq!(raw_response(response).await.1["grpc-status"], "0");
                    let neighbor = state.wait(2).await; neighbor.reclaimed().await;
                    assert!(neighbor.status().unwrap().error.is_none());
                }
                handle.shutdown(); drop(client); server.await.unwrap().unwrap();
                if let Some(driver) = driver { let _ = driver.await; }
                let expected = if action == "disconnect" { 1 } else { 2 };
                assert_eq!(hooks.opened.load(Ordering::Relaxed), expected);
                assert_eq!(hooks.finished.load(Ordering::Relaxed), expected);
                assert_eq!(hooks.violations.load(Ordering::Relaxed), 0, "{thrift} {action}");
                assert_eq!((handle.stats().active, handle.stats().active_calls, handle.stats().active_tasks), (0, 0, 0));
            }
        }
    }).await.unwrap();
}

#[tokio::test]
async fn unified_ttstream_native_open_prepares_metadata_before_common_factory_and_hooks() {
    struct PreparedMarker;
    struct Prepare;
    impl volo_thrift::streaming::StreamHook for Prepare {
        fn on_open(
            &self,
            cx: &mut volo_thrift::streaming::StreamContext,
        ) -> Result<(), volo_thrift::streaming::StreamError> {
            cx.update_extensions(|extensions| {
                extensions.insert(PreparedMarker);
            });
            cx.update_metadata(|metadata| {
                metadata.insert("native-ready", bytes::Bytes::from_static(b"ready"))
            });
            Ok(())
        }
    }
    struct Observe(Arc<AtomicUsize>);
    struct Hook;
    impl volo_streamx::StreamHook for Hook {
        fn on_open(&self, cx: &volo_streamx::StreamContext) -> volo_streamx::RpcResult<()> {
            assert!(cx.with_extensions(|extensions| extensions.contains::<PreparedMarker>()));
            assert_eq!(
                cx.metadata().get("native-ready").unwrap().as_ref(),
                b"ready"
            );
            Ok(())
        }
    }
    impl volo_streamx::StreamHookFactory for Observe {
        fn create(&self, cx: &volo_streamx::StreamContext) -> Arc<dyn volo_streamx::StreamHook> {
            assert!(cx.with_extensions(|extensions| extensions.contains::<PreparedMarker>()));
            self.0.fetch_add(1, Ordering::Relaxed);
            Arc::new(Hook)
        }
    }
    tokio::time::timeout(Duration::from_secs(4), async {
        let created = Arc::new(AtomicUsize::new(0));
        let prepared = volo_streamx::Server::new()
            .add_service(chat_service_streamx::BoundService::from_arc(chat()))
            .thrift(|server| server.streaming_hook(Arc::new(Prepare)))
            .stream_hook(Observe(created.clone()))
            .graceful_shutdown_timeout(Duration::from_millis(200))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let client = ChatServiceClientBuilder::new("chat")
            .address(address)
            .build();
        let (mut input, mut output) = client.talk().await.unwrap().into_split();
        input.send(&message(1)).await.unwrap();
        input.close_send().await.unwrap();
        assert!(output.recv().await.unwrap().is_some());
        assert!(output.recv().await.unwrap().is_none());
        handle.shutdown();
        server.await.unwrap().unwrap();
        assert_eq!(created.load(Ordering::Relaxed), 1);
        assert_eq!(
            (handle.stats().active_calls, handle.stats().active_tasks),
            (0, 0)
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unified_ttstream_native_open_rejection_still_finishes_common_observers() {
    struct Reject;
    impl volo_thrift::streaming::StreamHook for Reject {
        fn on_open(
            &self,
            _: &mut volo_thrift::streaming::StreamContext,
        ) -> Result<(), volo_thrift::streaming::StreamError> {
            Err(volo_thrift::streaming::StreamError::Application(
                pilota::thrift::ApplicationException::new(
                    pilota::thrift::ApplicationExceptionKind::UNKNOWN_METHOD,
                    "native open rejection",
                ),
            ))
        }
    }
    struct Observe(Arc<AtomicUsize>, Arc<AtomicUsize>);
    struct Hook(Arc<AtomicUsize>);
    impl volo_streamx::StreamHook for Hook {
        fn on_finish(&self, _: &volo_streamx::StreamContext, status: &volo_streamx::StreamStatus) {
            assert!(status.error.is_some());
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    impl volo_streamx::StreamHookFactory for Observe {
        fn create(&self, _: &volo_streamx::StreamContext) -> Arc<dyn volo_streamx::StreamHook> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Arc::new(Hook(self.1.clone()))
        }
    }
    tokio::time::timeout(Duration::from_secs(4), async {
        let created = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        let business = chat();
        let prepared = volo_streamx::Server::new()
            .add_service(chat_service_streamx::BoundService::from_arc(
                business.clone(),
            ))
            .thrift(|server| server.streaming_hook(Arc::new(Reject)))
            .stream_hook(Observe(created.clone(), finished.clone()))
            .graceful_shutdown_timeout(Duration::from_millis(200))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let client = ChatServiceClientBuilder::new("chat")
            .address(address)
            .build();
        let (mut input, mut output) = client.talk().await.unwrap().into_split();
        let _ = input.send(&message(1)).await;
        let _ = input.close_send().await;
        assert!(output.recv().await.is_err());
        handle.shutdown();
        server.await.unwrap().unwrap();
        assert_eq!(business.calls.load(Ordering::Relaxed), 0);
        assert_eq!(created.load(Ordering::Relaxed), 1);
        assert_eq!(finished.load(Ordering::Relaxed), 1);
        assert_eq!(
            (handle.stats().active_calls, handle.stats().active_tasks),
            (0, 0)
        );
    })
    .await
    .unwrap();
}
