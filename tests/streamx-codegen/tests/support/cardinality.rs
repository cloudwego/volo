//! Raw wire requests compare direct generated bindings in native and mixed entries.
use super::*;

#[derive(Default)]
struct Handler([AtomicUsize; 4]);

macro_rules! stream_methods {
    ($message:ty) => {
        async fn upload(
            &self,
            _: StreamContext,
            mut input: RequestStream<$message>,
        ) -> RpcResult<$message> {
            self.0[1].fetch_add(1, Ordering::Relaxed);
            let mut reply = <$message>::default();
            while input.recv().await?.is_some() {
                reply.sequence += 1;
            }
            Ok(reply)
        }
        async fn watch(
            &self,
            _: StreamContext,
            request: $message,
            mut output: ResponseSender<$message>,
        ) -> RpcResult<()> {
            self.0[2].fetch_add(1, Ordering::Relaxed);
            output.send(&request).await
        }
        async fn talk(
            &self,
            _: StreamContext,
            mut input: RequestStream<$message>,
            mut output: ResponseSender<$message>,
        ) -> RpcResult<()> {
            self.0[3].fetch_add(1, Ordering::Relaxed);
            while let Some(request) = input.recv().await? {
                output.send(&request).await?;
            }
            Ok(())
        }
    };
}

impl chat_service_streamx::Handler for Handler {
    async fn get(
        &self,
        _: StreamContext,
        request: Message,
    ) -> RpcResult<Result<Message, ChatServiceGetException>> {
        self.0[0].fetch_add(1, Ordering::Relaxed);
        Ok(Ok(request))
    }
    async fn ping(&self, _: StreamContext, _: ()) -> RpcResult<()> {
        Ok(())
    }
    async fn add(&self, _: StreamContext, _: ChatServiceAddArgsRecv) -> RpcResult<i64> {
        Ok(0)
    }
    async fn fire(&self, _: StreamContext, _: Message) -> RpcResult<()> {
        Ok(())
    }
    stream_methods!(Message);
}
impl pb::presence_service_streamx::Handler for Handler {
    async fn get(&self, _: StreamContext, request: pb::Event) -> RpcResult<pb::Event> {
        self.0[0].fetch_add(1, Ordering::Relaxed);
        Ok(request)
    }
    stream_methods!(pb::Event);
}

#[tokio::test]
async fn h2_request_cardinality_and_empty_terminals_match_native_entry() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for mixed in [false, true] {
            let handler = Arc::new(Handler::default());
            let thrift = chat_service_streamx::BoundService::from_arc(handler.clone());
            let proto = pb::presence_service_streamx::BoundService::from_arc(handler.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (stop, stopped) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                if mixed {
                    volo_streamx::Server::new()
                        .add_fallback_service(thrift)
                        .add_service(proto)
                        .run_with_shutdown(DefaultIncoming::from(listener), async {
                            stopped.await.unwrap();
                            Ok(())
                        })
                        .await
                        .unwrap();
                } else {
                    volo_grpc::server::Server::new()
                        .add_service(volo_grpc::server::ServiceBuilder::new(thrift.grpc()).build())
                        .add_service(volo_grpc::server::ServiceBuilder::new(proto.grpc()).build())
                        .run_with_shutdown(DefaultIncoming::from(listener), async {
                            stopped.await.unwrap();
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
            for thrift in [true, false] {
                let (service, content_type, raw, unary) = if thrift {
                    (
                        chat_service_streamx::DESCRIPTOR.wire_name,
                        "application/grpc+thrift",
                        thrift_bytes(&message(7)),
                        thrift_bytes(&ChatServiceGetArgsRecv {
                            request: message(7),
                        }),
                    )
                } else {
                    let data: bytes::Bytes = pilota::pb::Message::encode_to_vec(
                        &pb::Event {
                            text: "first".into(),
                            sequence: 7,
                        },
                        &mut Default::default(),
                    )
                    .into();
                    (
                        pb::presence_service_streamx::DESCRIPTOR.wire_name,
                        "application/grpc",
                        data.clone(),
                        data,
                    )
                };
                // An empty stream and a single zero-byte message are distinct inputs.
                for (method, messages, status, response_messages, index) in [
                    ("Get", vec![], "3", 0, 0),
                    ("Get", vec![unary.clone(), unary.clone()], "3", 0, 0),
                    ("Watch", vec![], "3", 0, 2),
                    ("Watch", vec![raw.clone(), raw.clone()], "0", 1, 2),
                    ("Upload", vec![], "0", 1, 1),
                    ("Talk", vec![], "0", 0, 3),
                    ("Talk", vec![raw.clone(), raw.clone()], "0", 2, 3),
                    (
                        "Get",
                        vec![bytes::Bytes::new()],
                        if thrift { "13" } else { "0" },
                        if thrift { 0 } else { 1 },
                        0,
                    ),
                    ("Get", vec![unary.clone()], "0", 1, 0),
                ] {
                    let before = handler.0[index].load(Ordering::Relaxed);
                    let request = http::Request::builder()
                        .method("POST")
                        .uri(format!("http://{address}/{service}/{method}"))
                        .header("content-type", content_type)
                        .header("te", "trailers")
                        .body(())
                        .unwrap();
                    let empty = messages.is_empty();
                    let (response, mut upload) = client.send_request(request, empty).unwrap();
                    if !empty {
                        let data: Vec<_> = messages
                            .into_iter()
                            .flat_map(|value| grpc_frame(value).to_vec())
                            .collect();
                        upload.send_data(data.into(), true).unwrap();
                    }
                    let (http, headers, data) = raw_response(response).await;
                    assert_eq!(http, http::StatusCode::OK);
                    assert_eq!(headers["content-type"], content_type);
                    assert_eq!(
                        headers["grpc-status"], status,
                        "mixed={mixed} {service}/{method}"
                    );
                    let mut remaining = data.as_ref();
                    let mut count = 0;
                    while !remaining.is_empty() {
                        assert!(remaining.len() >= 5);
                        assert_eq!(remaining[0], 0);
                        let length =
                            u32::from_be_bytes(remaining[1..5].try_into().unwrap()) as usize;
                        assert!(remaining.len() >= length + 5);
                        if method == "Watch" || method == "Talk" {
                            assert_eq!(&remaining[5..5 + length], raw.as_ref());
                        }
                        remaining = &remaining[5 + length..];
                        count += 1;
                    }
                    assert_eq!(count, response_messages);
                    assert_eq!(
                        handler.0[index].load(Ordering::Relaxed) - before,
                        usize::from(status == "0")
                    );
                }
            }
            drop(client);
            stop.send(()).unwrap();
            server.await.unwrap();
            driver.await.unwrap().unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn ttstream_extra_data_and_early_trailer_match_native_entry() {
    use pilota::thrift::{ApplicationExceptionKind, Message as _};
    use tokio::io::AsyncWriteExt;
    use volo_thrift::{
        server::NamedService as _,
        transport::ttstream::frame::{Frame, FrameType, read_frame},
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        for mixed in [false, true] {
            let handler = Arc::new(Handler::default());
            let bound = chat_service_streamx::BoundService::from_arc(handler.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (stop, stopped) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                if mixed {
                    volo_streamx::Server::new()
                        .add_service(bound)
                        .run_with_shutdown(DefaultIncoming::from(listener), async {
                            let _ = stopped.await;
                            Ok(())
                        })
                        .await
                        .unwrap();
                } else {
                    let service = bound.thrift();
                    let router = service.streaming_router().unwrap();
                    let prepared = volo_thrift::server::Server::new(service)
                        .streaming_router(router)
                        .prepare()
                        .unwrap();
                    let (socket, _) = listener.accept().await.unwrap();
                    prepared.connection().serve(socket.into()).await;
                }
            });
            let config = volo_thrift::streaming::StreamConfig::default();
            let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
            for (n, (method, requests, responses, index, error)) in [
                ("Watch", 2, 1, 2, false),
                ("Watch", 0, 0, 2, true),
                ("Upload", 0, 1, 1, false),
                ("Talk", 0, 0, 3, false),
                ("Talk", 2, 2, 3, false),
                ("Watch", 1, 1, 2, false),
            ]
            .into_iter()
            .enumerate()
            {
                let before = handler.0[index].load(Ordering::Relaxed);
                let sid = -100 - n as i32;
                let method = bytes::Bytes::copy_from_slice(method.as_bytes());
                let mut wire = Vec::new();
                for (kind, data) in std::iter::once((FrameType::Header, bytes::Bytes::new()))
                    .chain((0..requests).map(|i| (FrameType::Data, thrift_bytes(&message(i)))))
                    .chain(std::iter::once((FrameType::Trailer, bytes::Bytes::new())))
                {
                    wire.extend_from_slice(
                        &Frame::new(sid, kind, method.clone(), data)
                            .encode(&config)
                            .unwrap(),
                    );
                }
                socket.write_all(&wire).await.unwrap();
                let mut messages = 0;
                let mut headers = 0;
                loop {
                    let frame = read_frame(&mut socket, &config).await.unwrap().unwrap();
                    assert_eq!(frame.stream_id, sid);
                    match frame.kind {
                        FrameType::Header => headers += 1,
                        FrameType::Data => {
                            let mut data = frame.payload;
                            let message = Message::decode(
                                &mut pilota::thrift::binary::TBinaryProtocol::new(&mut data, true),
                            )
                            .unwrap();
                            assert_eq!(
                                message.sequence,
                                if index == 1 { requests } else { messages }
                            );
                            messages += 1;
                        }
                        FrameType::Trailer => {
                            if error {
                                let exception =
                                    volo_thrift::streaming::decode_exception(frame.payload)
                                        .unwrap();
                                assert_eq!(
                                    exception.kind(),
                                    ApplicationExceptionKind::INTERNAL_ERROR
                                );
                            } else {
                                assert!(frame.payload.is_empty());
                            }
                            break;
                        }
                        kind => panic!("unexpected {kind:?}, mixed={mixed}, method={method:?}"),
                    }
                }
                assert!(headers <= 1);
                assert_eq!(messages, responses);
                assert_eq!(
                    handler.0[index].load(Ordering::Relaxed) - before,
                    usize::from(!error)
                );
            }
            drop(socket);
            let _ = stop.send(());
            server.await.unwrap();
        }
    })
    .await
    .unwrap();
}
