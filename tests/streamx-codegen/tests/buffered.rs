use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use pilota::thrift::{
    ApplicationException, ApplicationExceptionKind, Message as _, TAsyncBinaryProtocol,
    TAsyncCompactProtocol, TAsyncInputProtocol, TMessageIdentifier, TMessageType, TOutputProtocol,
    binary::TBinaryProtocol, compact::TCompactOutputProtocol,
};
use streamx_codegen::thrift::chat::*;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    sync::Notify,
};
use volo_streamx::{RequestStream, ResponseSender, RpcResult, StreamContext};
use volo_thrift::server::DetectedThrift;

#[derive(Default)]
struct State {
    started: Notify,
    release: Notify,
    added: Notify,
    calls: AtomicUsize,
    fires: AtomicUsize,
}

struct Chat(Arc<State>);
impl chat_service_streamx::Handler for Chat {
    async fn get(
        &self,
        _: StreamContext,
        request: Message,
    ) -> RpcResult<Result<Message, ChatServiceGetException>> {
        self.0.calls.fetch_add(1, Ordering::Relaxed);
        if request.sequence == -1 {
            return Ok(Err(ChatServiceGetException::Missing(Missing {
                reason: "missing".into(),
            })));
        }
        Ok(Ok(request))
    }
    async fn ping(&self, _: StreamContext, _: ()) -> RpcResult<()> {
        Ok(())
    }
    async fn add(&self, _: StreamContext, request: ChatServiceAddArgsRecv) -> RpcResult<i64> {
        self.0.calls.fetch_add(1, Ordering::Relaxed);
        self.0.added.notify_one();
        Ok(i64::from(request.left) + request.right)
    }
    async fn fire(&self, _: StreamContext, _: Message) -> RpcResult<()> {
        self.0.fires.fetch_add(1, Ordering::Relaxed);
        Err(volo_streamx::RpcError::Biz {
            code: 17,
            message: "oneway".into(),
            extra: Default::default(),
        })
    }
    async fn upload(&self, _: StreamContext, _: RequestStream<Message>) -> RpcResult<Message> {
        unreachable!()
    }
    async fn watch(
        &self,
        _: StreamContext,
        _: Message,
        _: ResponseSender<Message>,
    ) -> RpcResult<()> {
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

struct Archive(Arc<State>);
impl archive_service_streamx::Handler for Archive {
    async fn get(&self, cx: StreamContext, mut request: Message) -> RpcResult<Message> {
        assert_eq!(cx.service(), "ArchiveService");
        assert_eq!(StreamContext::current().unwrap().id(), cx.id());
        if request.text == "block" {
            self.0.started.notify_one();
            self.0.release.notified().await;
            assert!(cx.cancellation().is_none());
        }
        self.0.calls.fetch_add(1, Ordering::Relaxed);
        request.text = format!("archive:{}", request.text).into();
        Ok(request)
    }
}

fn registry(state: &Arc<State>) -> Arc<volo_streamx::FrozenRegistry> {
    let mut registry = volo_streamx::Registry::new();
    registry
        .register(chat_service_streamx::bind(Chat(state.clone())))
        .unwrap();
    registry
        .register(archive_service_streamx::bind(Archive(state.clone())))
        .unwrap();
    registry.fallback("ArchiveService");
    registry.freeze().unwrap()
}

async fn connection(
    state: &Arc<State>,
    limit: i32,
) -> (BufReader<TcpStream>, tokio::task::JoinHandle<()>) {
    use volo_thrift::codec::default::{
        DefaultMakeCodec, framed::MakeFramedCodec, thrift::MakeThriftCodec,
        ttheader::MakeTTHeaderCodec,
    };
    let directory = registry(state);
    let codec = DefaultMakeCodec::new(MakeTTHeaderCodec::new(
        MakeFramedCodec::new(MakeThriftCodec::new()).with_max_frame_size(limit),
    ));
    connection_with_codec(directory, codec).await
}

async fn connection_with_codec<M>(
    directory: Arc<volo_streamx::FrozenRegistry>,
    codec: M,
) -> (BufReader<TcpStream>, tokio::task::JoinHandle<()>)
where
    M: volo_thrift::codec::MakeCodec<
            volo::net::conn::OwnedReadHalf,
            volo::net::conn::OwnedWriteHalf,
        >,
{
    let server = volo_thrift::server::Server::new(directory.thrift_service())
        .make_codec(directory.thrift_codec(codec));
    #[cfg(feature = "multiplex")]
    let server = server.multiplex(true);
    let prepared = server.prepare().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let connection = volo::net::conn::Conn::from(stream);
        #[cfg(feature = "shmipc")]
        let _guard = volo::net::ext::AsyncExt::shmipc_helper(&connection).close_guard();
        let peeked = volo::net::conn::PeekedConn::peek8(connection)
            .await
            .unwrap()
            .unwrap();
        prepared
            .connection()
            .serve_detected(peeked.into_buffered_split(8192), DetectedThrift::Unary)
            .await
            .unwrap();
    });
    (
        BufReader::new(TcpStream::connect(address).await.unwrap()),
        task,
    )
}

#[derive(Default)]
struct WriteGate {
    started: Notify,
    release: Notify,
    attempts: AtomicUsize,
}

#[derive(Clone)]
struct GatedCodec<M> {
    inner: M,
    gate: Arc<WriteGate>,
}
struct GatedEncoder<E> {
    inner: E,
    gate: Arc<WriteGate>,
}

impl<E: volo_thrift::codec::Encoder> volo_thrift::codec::Encoder for GatedEncoder<E> {
    async fn encode<M: volo_thrift::EntryMessage, Cx: volo_thrift::context::ThriftContext>(
        &mut self,
        cx: &mut Cx,
        message: volo_thrift::ThriftMessage<M>,
    ) -> Result<(), pilota::thrift::ThriftException> {
        if self.gate.attempts.fetch_add(1, Ordering::Relaxed) == 0 {
            self.gate.started.notify_one();
            self.gate.release.notified().await;
        }
        self.inner.encode(cx, message).await
    }
}

impl<M>
    volo_thrift::codec::MakeCodec<volo::net::conn::OwnedReadHalf, volo::net::conn::OwnedWriteHalf>
    for GatedCodec<M>
where
    M: volo_thrift::codec::MakeCodec<
            volo::net::conn::OwnedReadHalf,
            volo::net::conn::OwnedWriteHalf,
        >,
{
    type Encoder = GatedEncoder<M::Encoder>;
    type Decoder = M::Decoder;
    fn supports_buffered(&self) -> bool {
        self.inner.supports_buffered()
    }
    fn make_codec(
        &self,
        reader: volo::net::conn::OwnedReadHalf,
        writer: volo::net::conn::OwnedWriteHalf,
    ) -> (Self::Encoder, Self::Decoder) {
        let (inner, decoder) = self.inner.make_codec(reader, writer);
        (
            GatedEncoder {
                inner,
                gate: self.gate.clone(),
            },
            decoder,
        )
    }
    fn make_codec_from_buffered(
        &self,
        reader: volo::util::buf_reader::BufReader<volo::net::conn::OwnedReadHalf>,
        writer: volo::net::conn::OwnedWriteHalf,
    ) -> std::io::Result<(Self::Encoder, Self::Decoder)> {
        let (inner, decoder) = self.inner.make_codec_from_buffered(reader, writer)?;
        Ok((
            GatedEncoder {
                inner,
                gate: self.gate.clone(),
            },
            decoder,
        ))
    }
}

#[tokio::test]
async fn buffered_waits_for_response_write_before_dispatching_next_request() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let state = Arc::new(State::default());
        let gate = Arc::new(WriteGate::default());
        let codec = GatedCodec {
            inner: volo_thrift::codec::DefaultMakeCodec::default(),
            gate: gate.clone(),
        };
        let (mut io, task) = connection_with_codec(registry(&state), codec).await;
        io.get_mut()
            .write_all(&[get(1, false, "write-block"), add(2, false)].concat())
            .await
            .unwrap();
        gate.started.notified().await;
        assert_eq!(
            state.calls.load(Ordering::Relaxed),
            1,
            "first handler has returned"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), state.added.notified())
                .await
                .is_err()
        );
        gate.release.notify_one();
        assert_eq!(reply(&mut io, false, false).await.0.sequence_number, 1);
        assert_eq!(
            reply(&mut io, false, false).await,
            (
                TMessageIdentifier::new("Add".into(), TMessageType::Reply, 2),
                Value::Number(42)
            )
        );
        drop(io);
        task.await.unwrap();
    })
    .await
    .unwrap();
}

fn wire_header(method: &str, sequence: i32, oneway: bool, compact: bool) -> Vec<u8> {
    let identifier = TMessageIdentifier::new(
        method.to_owned().into(),
        if oneway {
            TMessageType::OneWay
        } else {
            TMessageType::Call
        },
        sequence,
    );
    let mut bytes = pilota::LinkedBytes::new();
    if compact {
        TCompactOutputProtocol::new(&mut bytes, true)
            .write_message_begin(&identifier)
            .unwrap();
    } else {
        TBinaryProtocol::new(&mut bytes, true)
            .write_message_begin(&identifier)
            .unwrap();
    }
    bytes.into_bytes_mut().to_vec()
}

fn request<M: pilota::thrift::Message>(
    method: &str,
    sequence: i32,
    oneway: bool,
    compact: bool,
    body: &M,
) -> Vec<u8> {
    let mut header = wire_header(method, sequence, oneway, compact);
    let mut bytes = pilota::LinkedBytes::new();
    if compact {
        body.encode(&mut TCompactOutputProtocol::new(&mut bytes, true))
            .unwrap();
    } else {
        body.encode(&mut TBinaryProtocol::new(&mut bytes, true))
            .unwrap();
    }
    header.extend_from_slice(&bytes.into_bytes_mut());
    header
}

fn get(sequence: i32, compact: bool, text: &str) -> Vec<u8> {
    request(
        "Get",
        sequence,
        false,
        compact,
        &ChatServiceGetArgsRecv {
            request: Message {
                text: text.to_owned().into(),
                sequence: 1,
            },
        },
    )
}

fn add(sequence: i32, compact: bool) -> Vec<u8> {
    request(
        "Add",
        sequence,
        false,
        compact,
        &ChatServiceAddArgsRecv {
            left: 17,
            right: 25,
        },
    )
}

fn framed(message: &[u8]) -> Vec<u8> {
    let mut bytes = (message.len() as i32).to_be_bytes().to_vec();
    bytes.extend_from_slice(message);
    bytes
}

#[derive(Debug, PartialEq)]
enum Value {
    Message(String),
    Number(i64),
    Empty,
    Unknown,
}

async fn read_reply<P: TAsyncInputProtocol>(p: &mut P) -> (TMessageIdentifier, Value) {
    let ident = p.read_message_begin().await.unwrap();
    let value = if ident.message_type == TMessageType::Exception {
        let error = ApplicationException::decode_async(p).await.unwrap();
        assert_eq!(error.kind(), ApplicationExceptionKind::UNKNOWN_METHOD);
        Value::Unknown
    } else {
        match ident.name.as_str() {
            "Get" => match ChatServiceGetResultRecv::decode_async(p).await.unwrap() {
                ChatServiceGetResultRecv::Ok(message) => Value::Message(message.text.to_string()),
                result => panic!("unexpected result {result:?}"),
            },
            "Add" => match ChatServiceAddResultRecv::decode_async(p).await.unwrap() {
                ChatServiceAddResultRecv::Ok(value) => Value::Number(value),
            },
            "Ping" => {
                ChatServicePingResultRecv::decode_async(p).await.unwrap();
                Value::Empty
            }
            method => panic!("unexpected method {method}"),
        }
    };
    p.read_message_end().await.unwrap();
    (ident, value)
}

async fn reply(
    reader: &mut BufReader<TcpStream>,
    compact: bool,
    has_frame: bool,
) -> (TMessageIdentifier, Value) {
    let mut frame = Vec::new();
    if has_frame {
        let size = reader.read_i32().await.unwrap();
        assert!((1..8192).contains(&size));
        frame.resize(size as usize, 0);
        reader.read_exact(&mut frame).await.unwrap();
    }
    let input: &mut (dyn AsyncRead + Send + Unpin) = if has_frame {
        &mut frame.as_slice()
    } else {
        reader
    };
    if compact {
        read_reply(&mut TAsyncCompactProtocol::new(input)).await
    } else {
        read_reply(&mut TAsyncBinaryProtocol::new(input)).await
    }
}

#[tokio::test]
async fn buffered_binary_and_compact_are_serial_across_services() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for compact in [false, true] {
            let state = Arc::new(State::default());
            let (mut io, task) = connection(&state, 8192).await;
            let first = get(-7, compact, "block");
            io.get_mut().write_all(&first[..1]).await.unwrap();
            io.get_mut().write_all(&first[1..8]).await.unwrap();
            io.get_mut()
                .write_all(&[first[8..].to_vec(), add(31, compact)].concat())
                .await
                .unwrap();
            state.started.notified().await;
            assert!(
                tokio::time::timeout(Duration::from_millis(50), state.added.notified())
                    .await
                    .is_err()
            );
            assert_eq!(state.calls.load(Ordering::Relaxed), 0);
            state.release.notify_one();
            let (ident, value) = reply(&mut io, compact, false).await;
            assert_eq!(ident.name, "Get");
            assert_eq!(ident.sequence_number, -7);
            assert_eq!(value, Value::Message("archive:block".into()));
            let (ident, value) = reply(&mut io, compact, false).await;
            assert_eq!(ident.name, "Add");
            assert_eq!(ident.sequence_number, 31);
            assert_eq!(value, Value::Number(42));
            let fire = request(
                "Fire",
                32,
                true,
                compact,
                &ChatServiceFireArgsRecv {
                    request: Message {
                        text: "fire".into(),
                        sequence: 1,
                    },
                },
            );
            let ping = request("Ping", 33, false, compact, &ChatServicePingArgsRecv {});
            io.get_mut()
                .write_all(&[fire, ping].concat())
                .await
                .unwrap();
            let (ident, value) = reply(&mut io, compact, false).await;
            assert_eq!(ident.name, "Ping");
            assert_eq!(ident.sequence_number, 33);
            assert_eq!(value, Value::Empty);
            assert_eq!(state.fires.load(Ordering::Relaxed), 1);
            assert_eq!(state.calls.load(Ordering::Relaxed), 2);
            drop(io);
            task.await.unwrap();
        }
    })
    .await
    .unwrap();
}

fn unknown_at_limit(limit: usize, oneway: bool, compact: bool) -> Vec<u8> {
    let mut bytes = wire_header("Missing", 77, oneway, compact);
    let payload_size = limit - bytes.len() - if compact { 2 + 17 + 1 } else { 7 + 19 + 1 };
    if compact {
        assert!(payload_size < 128);
        bytes.extend_from_slice(&[0x18, payload_size as u8]);
    } else {
        bytes.extend_from_slice(&[11, 0, 1]);
        bytes.extend_from_slice(&(payload_size as i32).to_be_bytes());
    }
    bytes.extend(vec![0xff; payload_size]);
    if compact {
        bytes.push(0x1d);
    } else {
        bytes.extend_from_slice(&[16, 0, 2]);
    }
    bytes.extend_from_slice(&[7; 16]);
    bytes.push(0);
    assert_eq!(bytes.len(), limit);
    bytes
}

#[tokio::test]
async fn unknown_skip_includes_identifier_and_preserves_next_request() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for oneway in [false, true] {
            for compact in [false, true] {
                let state = Arc::new(State::default());
                let (mut io, task) = connection(&state, 128).await;
                io.get_mut()
                    .write_all(&[unknown_at_limit(128, oneway, compact), add(78, compact)].concat())
                    .await
                    .unwrap();
                if !oneway {
                    let (ident, value) = reply(&mut io, compact, false).await;
                    assert_eq!(ident.name, "Missing");
                    assert_eq!(ident.sequence_number, 77);
                    assert_eq!(value, Value::Unknown);
                }
                let (ident, value) = reply(&mut io, compact, false).await;
                assert_eq!(ident.name, "Add");
                assert_eq!(ident.sequence_number, 78);
                assert_eq!(value, Value::Number(42));
                assert_eq!(state.calls.load(Ordering::Relaxed), 1);
                drop(io);
                task.await.unwrap();
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn oversized_malformed_and_truncated_requests_close_without_dispatch() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut truncated = unknown_at_limit(128, true, false);
        truncated.pop();
        let mut huge_name = vec![0x80, 1, 0, 1];
        huge_name.extend_from_slice(&i32::MAX.to_be_bytes());
        let mut huge_field = wire_header("Get", 80, false, false);
        huge_field.extend_from_slice(&[12, 0, 1, 11, 0, 1]);
        huge_field.extend_from_slice(&i32::MAX.to_be_bytes());
        let mut negative = wire_header("Missing", 81, true, false);
        negative.extend_from_slice(&[11, 0, 1, 0xff, 0xff, 0xff, 0xff]);
        for (bytes, append_next, no_response) in [
            (unknown_at_limit(129, true, false), true, true),
            (unknown_at_limit(129, true, true), true, true),
            (truncated, false, true),
            (huge_name, false, false),
            (huge_field, false, false),
            (negative, false, true),
        ] {
            let state = Arc::new(State::default());
            let (mut io, task) = connection(&state, 128).await;
            io.get_mut().write_all(&bytes).await.unwrap();
            if append_next {
                io.get_mut().write_all(&add(82, false)).await.unwrap();
            }
            io.get_mut().shutdown().await.unwrap();
            let mut response = Vec::new();
            match io.read_to_end(&mut response).await {
                Ok(_) => {}
                Err(error) => assert!(matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                )),
            }
            if no_response {
                assert!(response.is_empty(), "oneway must never return an exception");
            }
            task.await.unwrap();
            assert_eq!(state.calls.load(Ordering::Relaxed), 0);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn framed_unknown_method_preserves_response_order_and_seqid() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for compact in [false, true] {
            let state = Arc::new(State::default());
            let (mut io, task) = connection(&state, 128).await;
            let unknown = framed(&unknown_at_limit(128, false, compact));
            // Wait for the unknown response so this assertion also applies with multiplex enabled.
            io.get_mut().write_all(&unknown).await.unwrap();
            let (ident, value) = reply(&mut io, compact, true).await;
            assert_eq!(ident.name, "Missing");
            assert_eq!(ident.sequence_number, 77);
            assert_eq!(value, Value::Unknown);
            io.get_mut()
                .write_all(&framed(&add(79, compact)))
                .await
                .unwrap();
            let (ident, value) = reply(&mut io, compact, true).await;
            assert_eq!(ident.sequence_number, 79);
            assert_eq!(value, Value::Number(42));
            drop(io);
            task.await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[cfg(feature = "multiplex")]
#[tokio::test]
async fn framed_requests_still_multiplex_and_finish_after_peer_write_eof() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for compact in [false, true] {
            let state = Arc::new(State::default());
            let (mut io, task) = connection(&state, 8192).await;
            io.get_mut()
                .write_all(
                    &[
                        framed(&get(90, compact, "block")),
                        framed(&add(91, compact)),
                    ]
                    .concat(),
                )
                .await
                .unwrap();
            state.started.notified().await;
            state.added.notified().await;
            let (ident, value) = reply(&mut io, compact, true).await;
            assert_eq!(ident.name, "Add");
            assert_eq!(ident.sequence_number, 91);
            assert_eq!(value, Value::Number(42));
            io.get_mut().shutdown().await.unwrap();
            // Admission has ended while Get is still blocked. The writer must remain owned.
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(
                !task.is_finished(),
                "the connection driver must retain accepted work"
            );
            state.release.notify_one();
            let (ident, value) = reply(&mut io, compact, true).await;
            assert_eq!(ident.name, "Get");
            assert_eq!(ident.sequence_number, 90);
            assert_eq!(value, Value::Message("archive:block".into()));
            task.await.unwrap();
        }
    })
    .await
    .unwrap();
}
