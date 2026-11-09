//! Compiled as a downstream crate: exercise only public codec APIs.
use std::{io, marker::PhantomPinned};

use bytes::{BufMut, Bytes};
use futures::{StreamExt, stream};
use http::{HeaderMap, HeaderValue, StatusCode};
use http_body::Frame;
use http_body_util::StreamBody;
use pilota::LinkedBytes;
use volo_grpc::{
    Code, RecvStream, Status,
    body::{Body, boxed, empty_body},
    codec::{
        CodecLimits, Decoder, Encoder,
        compression::CompressionEncoding,
        decode::Kind,
        encode::{encode_with_encoder, encode_with_options},
    },
};

type Reexported<T> = RecvStream<T>;
struct Wrapper<T>(Reexported<T>);

#[test]
fn status_retains_peer_code_without_large_results_or_private_headers() {
    assert!(std::mem::size_of::<Status>() <= 128);
    for (raw, expected) in [("20", Code::Unknown), ("1", Code::Cancelled)] {
        let mut headers = HeaderMap::new();
        headers.insert("grpc-status", raw.parse().unwrap());
        let status = Status::from_header_map(&headers).unwrap();
        assert_eq!(status.code(), expected);
        assert_eq!(status.raw_grpc_code(), Some(raw.as_bytes()));
        let wrapped = Status::from_error(Box::new(status));
        assert_eq!(wrapped.raw_grpc_code(), Some(raw.as_bytes()));
        assert_eq!(wrapped.to_header_map().unwrap().len(), 1);
        assert!(wrapped.metadata().is_empty());
    }
    assert!(Status::cancelled("local").raw_grpc_code().is_none());
    let mut status = Status::ok("");
    status
        .metadata_mut()
        .insert("x-status", "kept".parse().unwrap());
    assert_eq!(status.metadata().get("x-status").unwrap(), "kept");
}

#[test]
fn old_turbofish_alias_newtype_debug_and_unpin_need_no_message_bound() {
    struct NotAMessage(PhantomPinned);
    fn unpin<T: Unpin>(_: &T) {}
    fn send<T: Send>(_: &T) {}
    let value = RecvStream::<NotAMessage>::new(empty_body(), Kind::Request, None);
    unpin(&value);
    send(&value);
    assert_eq!(format!("{value:?}"), "RecvStream");
    let wrapper = Wrapper::<NotAMessage>(value);
    drop(wrapper.0);
}

#[derive(Debug, Default)]
struct PinnedProtobuf {
    value: volo_grpc::codec::encode::tests::EchoRequest,
    _pin: PhantomPinned,
}

impl pilota::pb::Message for PinnedProtobuf {
    fn encoded_len(&self, cx: &mut pilota::pb::EncodeLengthContext) -> usize {
        self.value.encoded_len(cx)
    }
    fn encode_raw(&self, dst: &mut LinkedBytes) {
        self.value.encode_raw(dst);
    }
    fn merge_field(
        &mut self,
        tag: u32,
        wire: pilota::pb::encoding::WireType,
        src: &mut Bytes,
        cx: &mut pilota::pb::encoding::DecodeContext,
        root: bool,
    ) -> Result<(), pilota::pb::DecodeError> {
        self.value.merge_field(tag, wire, src, cx, root)
    }
}

struct LegacySend(PinnedProtobuf);
struct LegacyRecv(RecvStream<PinnedProtobuf>);

impl volo_grpc::SendEntryMessage for LegacySend {
    fn into_body(
        self,
        compression: Option<CompressionEncoding>,
    ) -> volo_grpc::BoxStream<'static, Result<Frame<Bytes>, Status>> {
        volo_grpc::codec::encode::encode(stream::iter([Ok(self.0)]), compression)
    }
}
impl volo_grpc::RecvEntryMessage for LegacyRecv {
    fn from_body(
        _: Option<&str>,
        body: volo_grpc::body::BoxBody,
        kind: Kind,
        compression: Option<CompressionEncoding>,
    ) -> Result<Self, Status> {
        Ok(Self(RecvStream::new(body, kind, compression)))
    }
}

#[tokio::test]
async fn old_custom_entry_messages_support_pinned_protobuf_values() {
    use volo_grpc::{RecvEntryMessage, SendEntryMessage};
    let sender = LegacySend(PinnedProtobuf::default());
    let mut receiver = LegacyRecv::from_body(
        None,
        boxed(Body::new(sender.into_body(None))),
        Kind::Response(StatusCode::OK),
        None,
    )
    .unwrap();
    fn unpin<T: Unpin>(_: &T) {}
    unpin(&receiver.0);
    assert!(receiver.0.next().await.unwrap().is_ok());
    assert!(receiver.0.next().await.is_none());
}

#[tokio::test]
async fn trailers_only_ok_is_accepted_by_old_entry_messages_over_h2() {
    use motore::Service;
    use volo_grpc::{
        Request,
        client::Http2Config,
        context::{ClientContext, Config},
        transport::ClientTransport,
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut connection = h2::server::handshake(socket).await.unwrap();
            while let Some(request) = connection.accept().await {
                let (_, mut response) = request.unwrap();
                response
                    .send_response(
                        http::Response::builder()
                            .header("content-type", "application/grpc")
                            .header("grpc-status", "0")
                            .body(())
                            .unwrap(),
                        true,
                    )
                    .unwrap();
            }
        });
        let transport =
            ClientTransport::<LegacyRecv>::new(&Http2Config::default(), &Config::default());
        let mut cx = ClientContext::default();
        cx.rpc_info
            .callee_mut()
            .set_address(volo::net::Address::from(address));
        cx.rpc_info.set_method("/public.Probe/Empty".into());
        let request = Request::new(LegacySend(PinnedProtobuf::default()));
        let mut response = transport.call(&mut cx, request).await.unwrap().into_inner();
        assert!(
            response.0.next().await.is_none(),
            "initial status is already the terminal frame"
        );
        peer.abort();
        let _ = peer.await;
    })
    .await
    .unwrap();
}

// Neither a Protobuf message nor Default/Clone/Sync. The received value also
// need not be Unpin; it is never pinned as a decoder field.
struct Value {
    byte: u8,
    _not_sync: std::cell::Cell<()>,
    _pin: PhantomPinned,
}

impl Value {
    fn new(byte: u8) -> Self {
        Self {
            byte,
            _not_sync: std::cell::Cell::new(()),
            _pin: PhantomPinned,
        }
    }
}

struct StatefulEncoder(u8);
impl Encoder for StatefulEncoder {
    type Item = Value;
    type Error = io::Error;
    fn encode(&mut self, item: Value, dst: &mut LinkedBytes) -> io::Result<()> {
        self.0 += 1;
        dst.put_slice(&[self.0, item.byte]);
        Ok(())
    }
}

struct StatefulDecoder(u8);
impl Decoder for StatefulDecoder {
    type Item = Value;
    type Error = io::Error;
    fn decode(&mut self, src: Bytes) -> io::Result<Option<Value>> {
        self.0 += 1;
        assert_eq!(src[0], self.0, "codec state must persist across messages");
        Ok(Some(Value::new(src[1])))
    }
}

async fn stateful_round_trip(compression: Option<CompressionEncoding>) {
    let source = stream::iter([Ok(Value::new(11)), Ok(Value::new(22))]);
    let frames = encode_with_encoder(source, compression, StatefulEncoder(0));
    let body = boxed(Body::new(frames));
    let mut received = RecvStream::with_decoder(
        body,
        Kind::Response(StatusCode::OK),
        compression,
        StatefulDecoder(0),
    );
    assert_eq!(received.message().await.unwrap().unwrap().byte, 11);
    assert_eq!(received.message().await.unwrap().unwrap().byte, 22);
    assert!(received.message().await.unwrap().is_none());
    assert!(received.next().await.is_none());
    assert_eq!(
        received
            .trailers()
            .await
            .unwrap()
            .unwrap()
            .get("grpc-status")
            .unwrap(),
        "0"
    );
}

#[tokio::test]
async fn custom_stateful_codec_identity() {
    stateful_round_trip(None).await;
    stateful_round_trip(Some(CompressionEncoding::Identity)).await;
}

#[cfg(feature = "gzip")]
#[tokio::test]
async fn custom_stateful_codec_gzip() {
    stateful_round_trip(Some(CompressionEncoding::Gzip(None))).await;
}

fn received(frames: Vec<Frame<Bytes>>) -> RecvStream<Value, StatefulDecoder> {
    RecvStream::with_decoder(
        boxed(StreamBody::new(stream::iter(
            frames.into_iter().map(Ok::<_, Status>),
        ))),
        Kind::Response(StatusCode::OK),
        None,
        StatefulDecoder(0),
    )
}

fn trailers(code: &'static str) -> Frame<Bytes> {
    let mut headers = HeaderMap::new();
    headers.insert("grpc-status", HeaderValue::from_static(code));
    headers.insert("x-final", HeaderValue::from_static("kept"));
    Frame::trailers(headers)
}

#[tokio::test]
async fn prefix_and_payload_can_split_at_every_byte_or_share_a_frame() {
    let wire = b"\x00\x00\x00\x00\x02\x01\x0b\x00\x00\x00\x00\x02\x02\x16";
    for chunk_size in 1..=wire.len() {
        let mut frames = wire
            .chunks(chunk_size)
            .map(|chunk| Frame::data(Bytes::copy_from_slice(chunk)))
            .collect::<Vec<_>>();
        frames.push(trailers("0"));
        let mut input = received(frames);
        assert_eq!(input.next().await.unwrap().unwrap().byte, 11);
        assert_eq!(input.next().await.unwrap().unwrap().byte, 22);
        assert!(input.next().await.is_none());
    }
}

#[tokio::test]
async fn terminal_errors_are_fused_and_retain_trailers() {
    let mut input = received(vec![trailers("13")]);
    assert_eq!(
        input.next().await.unwrap().err().unwrap().code(),
        Code::Internal
    );
    assert!(input.next().await.is_none());
    assert_eq!(
        input
            .trailers()
            .await
            .unwrap()
            .unwrap()
            .get("x-final")
            .unwrap(),
        "kept"
    );
    let mut missing = received(vec![]);
    assert_eq!(
        missing.next().await.unwrap().err().unwrap().code(),
        Code::Internal
    );
    assert!(missing.next().await.is_none());
    for wire in [b"\0\0".as_slice(), b"\0\0\0\0\x02", b"\0\0\0\0\x02\x01"] {
        let mut partial = received(vec![Frame::data(Bytes::copy_from_slice(wire))]);
        assert_eq!(
            partial.next().await.unwrap().err().unwrap().code(),
            Code::Internal
        );
        assert!(partial.next().await.is_none());
    }
}

#[tokio::test]
async fn malformed_status_details_in_final_trailers_do_not_panic_or_succeed() {
    for code in ["0", "13"] {
        let mut headers = HeaderMap::new();
        headers.insert("grpc-status", HeaderValue::from_static(code));
        headers.insert("grpc-status-details-bin", HeaderValue::from_static("%%%"));
        headers.insert("x-final", HeaderValue::from_static("kept"));
        let mut input = received(vec![Frame::trailers(headers)]);
        let error = input.next().await.unwrap().err().unwrap();
        assert_eq!(error.code(), Code::Internal);
        assert!(input.next().await.is_none());
        assert_eq!(
            input
                .trailers()
                .await
                .unwrap()
                .unwrap()
                .get("x-final")
                .unwrap(),
            "kept"
        );
    }
}

#[tokio::test]
async fn peer_length_is_checked_before_reserving_the_payload() {
    let mut input = received(vec![Frame::data(Bytes::from_static(b"\0\xff\xff\xff\xff"))])
        .with_limits(CodecLimits::default());
    assert_eq!(
        input.next().await.unwrap().err().unwrap().code(),
        Code::ResourceExhausted
    );
    assert!(input.next().await.is_none());
}

#[tokio::test]
async fn fatal_decode_releases_unread_io_while_the_stream_is_still_owned() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct IoGuard(Arc<AtomicBool>);
    impl Drop for IoGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = IoGuard(dropped.clone());
    let frames = async_stream::stream! {
        let _guard = guard;
        yield Ok::<_, Status>(Frame::data(Bytes::from_static(b"\0\xff\xff\xff\xff")));
        futures::future::pending::<()>().await;
    };
    let mut input = RecvStream::with_decoder(
        boxed(StreamBody::new(frames)),
        Kind::Request,
        None,
        StatefulDecoder(0),
    )
    .with_limits(CodecLimits::default());
    assert_eq!(
        input.next().await.unwrap().err().unwrap().code(),
        Code::ResourceExhausted
    );
    assert!(
        dropped.load(Ordering::Acquire),
        "unread IO must not wait for application Drop"
    );
    assert!(input.next().await.is_none());
}

#[tokio::test]
async fn decoder_can_skip_a_message_without_waiting_for_another_io_frame() {
    struct SkipFirst(u8);
    impl Decoder for SkipFirst {
        type Item = Value;
        type Error = io::Error;
        fn decode(&mut self, bytes: Bytes) -> io::Result<Option<Value>> {
            self.0 += 1;
            Ok((self.0 > 1).then(|| Value::new(bytes[1])))
        }
    }
    let frames = vec![
        Frame::data(Bytes::from_static(
            b"\0\0\0\0\x02\x01\x0b\0\0\0\0\x02\x02\x16",
        )),
        trailers("0"),
    ];
    let body = boxed(StreamBody::new(stream::iter(
        frames.into_iter().map(Ok::<_, Status>),
    )));
    let mut input =
        RecvStream::with_decoder(body, Kind::Response(StatusCode::OK), None, SkipFirst(0));
    assert_eq!(input.next().await.unwrap().unwrap().byte, 22);
    assert!(input.next().await.is_none());
}

#[tokio::test]
async fn sender_error_ends_the_stream_and_limit_is_reported_without_panic() {
    let source = stream::iter([Err(Status::aborted("producer")), Ok(Value::new(1))]);
    let mut output = encode_with_encoder(source, None, StatefulEncoder(0));
    assert_eq!(
        output.next().await.unwrap().unwrap_err().code(),
        Code::Aborted
    );
    assert!(output.next().await.is_none());
    let source = stream::iter([Ok(Value::new(1))]);
    let mut output = encode_with_options(
        source,
        None,
        StatefulEncoder(0),
        CodecLimits {
            max_send_message_size: 1,
            ..CodecLimits::default()
        },
    );
    assert_eq!(
        output.next().await.unwrap().unwrap_err().code(),
        Code::ResourceExhausted
    );
    assert!(output.next().await.is_none());
}

#[cfg(feature = "gzip")]
#[tokio::test]
async fn decompression_limit_is_checked_while_inflating() {
    let source = stream::iter([Ok(Value::new(1))]);
    let compression = Some(CompressionEncoding::Gzip(None));
    let body = boxed(Body::new(encode_with_encoder(
        source,
        compression,
        StatefulEncoder(0),
    )));
    let mut input = RecvStream::with_decoder(
        body,
        Kind::Response(StatusCode::OK),
        compression,
        StatefulDecoder(0),
    )
    .with_limits(CodecLimits {
        max_decompressed_message_size: 1,
        ..CodecLimits::default()
    });
    assert_eq!(
        input.next().await.unwrap().err().unwrap().code(),
        Code::ResourceExhausted
    );
    assert!(input.next().await.is_none());
}

#[cfg(feature = "thrift")]
#[tokio::test]
async fn thrift_binary_does_not_add_a_message_envelope() {
    use pilota::thrift::{ApplicationException, ApplicationExceptionKind};
    use volo_grpc::codec::thrift::{ThriftDecoder, ThriftEncoder};
    let value = ApplicationException::new(ApplicationExceptionKind::UNKNOWN_METHOD, "missing");
    let mut bytes = LinkedBytes::new();
    ThriftEncoder::default().encode(value, &mut bytes).unwrap();
    let mut bytes = bytes.concat().freeze();
    assert_eq!(bytes[0], 11); // field type STRING, not a Thrift message header
    let decoded = ThriftDecoder::<ApplicationException>::default()
        .decode(bytes.split_to(bytes.len()))
        .unwrap()
        .unwrap();
    assert_eq!(decoded.message(), "missing");
    assert_eq!(decoded.kind(), ApplicationExceptionKind::UNKNOWN_METHOD);
    assert!(
        ThriftDecoder::<ApplicationException>::default()
            .decode(Bytes::new())
            .is_err()
    );
}

// Keep the old encode() and RecvStream::<T> paths in a separate consumer crate.
#[tokio::test]
async fn original_protobuf_entry_points_still_interoperate() {
    use volo_grpc::codec::encode::{encode, tests::EchoRequest};
    let source = stream::iter([Ok(EchoRequest {
        message: "legacy".into(),
    })]);
    let body = boxed(Body::new(encode(source, None)));
    let mut input: Reexported<EchoRequest> =
        RecvStream::<EchoRequest>::new(body, Kind::Response(StatusCode::OK), None);
    assert_eq!(input.next().await.unwrap().unwrap().message, "legacy");
    assert!(input.next().await.is_none());
}
