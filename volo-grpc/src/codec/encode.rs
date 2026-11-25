use std::{
    mem,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{BufMut, Bytes};
use futures::Stream;
use http_body::Frame;
use linkedbytes::Node;
use pilota::{LinkedBytes, pb::Message};
use pin_project::pin_project;

use super::{DefaultEncoder, PREFIX_LEN};
use crate::{
    BoxStream, Status,
    codec::{
        BUFFER_SIZE, Encoder,
        compression::{CompressionEncoding, compress},
    },
};

// Match tonic's default soft threshold for batching ready messages.
// https://github.com/grpc/grpc-rust/blob/7053afcd3194e2b6f6ae52284ea17fc693fb07fc/tonic/src/codec/mod.rs#L51
const YIELD_THRESHOLD: usize = 32 * 1024;

pub fn encode<T, S>(
    source: S,
    compression_encoding: Option<CompressionEncoding>,
) -> BoxStream<'static, Result<Frame<Bytes>, Status>>
where
    S: Stream<Item = Result<T, Status>> + Send + 'static,
    T: Message + 'static,
{
    let compression_encoding = compression_encoding.filter(CompressionEncoding::is_enabled);
    Box::pin(EncodeStream {
        source: Some(source),
        compression_encoding,
        buffer: None,
        buffered_len: 0,
        state: State::Reading,
    })
}

// Ready-message batching follows tonic, adapted to preserve LinkedBytes nodes.
// https://github.com/grpc/grpc-rust/blob/7053afcd3194e2b6f6ae52284ea17fc693fb07fc/tonic/src/codec/encode.rs#L120-L153
#[pin_project]
struct EncodeStream<S> {
    #[pin]
    source: Option<S>,
    compression_encoding: Option<CompressionEncoding>,
    buffer: Option<LinkedBytes>,
    buffered_len: usize,
    state: State,
}

enum State {
    Reading,
    Flushing {
        node_index: usize,
        remaining: usize,
        error: Option<Status>,
        terminal: bool,
    },
    Done,
}

impl<T, S> Stream for EncodeStream<S>
where
    S: Stream<Item = Result<T, Status>>,
    T: Message,
{
    type Item = Result<Frame<Bytes>, Status>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        loop {
            if let State::Flushing {
                node_index,
                remaining,
                error,
                terminal,
            } = this.state
            {
                while *remaining > 0 {
                    let buffer = this
                        .buffer
                        .as_mut()
                        .expect("buffer exists while encoded bytes remain");
                    let node = match buffer.get_list_mut(*node_index) {
                        Some(node) => {
                            *node_index += 1;
                            mem::replace(node, Node::Bytes(Bytes::new()))
                        }
                        None => Node::BytesMut(mem::take(buffer.bytes_mut())),
                    };
                    let mut bytes = match node {
                        Node::Bytes(bytes) => bytes,
                        Node::BytesMut(bytes) => bytes.freeze(),
                        Node::FastStr(value) => value.into_bytes(),
                    };
                    // An encoder may append bytes before failing; emit only completed messages.
                    bytes.truncate(*remaining);
                    *remaining -= bytes.len();
                    if !bytes.is_empty() {
                        return Poll::Ready(Some(Ok(Frame::data(bytes))));
                    }
                }

                this.buffer.take();
                let error = error.take();
                *this.state = if *terminal {
                    State::Done
                } else {
                    State::Reading
                };
                if let Some(error) = error {
                    return Poll::Ready(Some(Err(error)));
                }
            }

            if matches!(this.state, State::Done) {
                this.source.set(None);
                return Poll::Ready(None);
            }

            let source = this
                .source
                .as_mut()
                .as_pin_mut()
                .expect("source exists while encoding");
            let (error, terminal) = match source.poll_next(cx) {
                Poll::Pending if *this.buffered_len == 0 => return Poll::Pending,
                Poll::Pending => (None, false),
                Poll::Ready(None) => (None, true),
                Poll::Ready(Some(Err(error))) => (Some(error), false),
                Poll::Ready(Some(Ok(item))) => {
                    let buffer = this
                        .buffer
                        .get_or_insert_with(|| LinkedBytes::with_capacity(BUFFER_SIZE));
                    match encode_item(item, buffer, *this.compression_encoding) {
                        Ok(len) => {
                            *this.buffered_len += len;
                            if *this.buffered_len < YIELD_THRESHOLD {
                                continue;
                            }
                            (None, false)
                        }
                        Err(error) => (Some(error), true),
                    }
                }
            };
            *this.state = State::Flushing {
                node_index: 0,
                remaining: mem::take(this.buffered_len),
                error,
                terminal,
            };
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if matches!(self.state, State::Done) {
            (0, Some(0))
        } else {
            (0, None)
        }
    }
}

fn encode_item<T: Message>(
    item: T,
    buffer: &mut LinkedBytes,
    compression_encoding: Option<CompressionEncoding>,
) -> Result<usize, Status> {
    let start = buffer.len();
    // A zero-copy insert may move the prefix into the node list.
    let prefix_node = buffer.iter_list().count();
    let prefix_offset = buffer.bytes().len();
    buffer.reserve(PREFIX_LEN);
    buffer.put_bytes(0, PREFIX_LEN);

    let mut encoder = DefaultEncoder::default();
    if let Some(config) = compression_encoding {
        let mut uncompressed = LinkedBytes::with_capacity(BUFFER_SIZE);
        encoder
            .encode(item, &mut uncompressed)
            .map_err(|err| Status::internal(format!("Error encoding: {err}")))?;
        compress(
            config,
            &mut uncompressed.into_bytes_mut(),
            buffer.bytes_mut(),
        )
        .map_err(|err| Status::internal(format!("Error compressing: {err}")))?;
    } else {
        encoder
            .encode(item, buffer)
            .map_err(|err| Status::internal(format!("Error encoding: {err}")))?;
    }

    let len = buffer.len() - start - PREFIX_LEN;
    assert!(len <= u32::MAX as usize);
    let mut prefix = match buffer.get_list_mut(prefix_node) {
        Some(Node::BytesMut(bytes)) => &mut bytes[prefix_offset..prefix_offset + PREFIX_LEN],
        Some(_) => unreachable!("message prefix is not in a mutable buffer"),
        None => &mut buffer.bytes_mut()[prefix_offset..prefix_offset + PREFIX_LEN],
    };
    prefix.put_u8(compression_encoding.is_some() as u8);
    prefix.put_u32(len as u32);
    Ok(PREFIX_LEN + len)
}

pub mod tests {
    #[cfg(test)]
    use futures::StreamExt;

    #[derive(Debug, Default, Clone, PartialEq)]
    pub struct EchoRequest {
        pub message: ::pilota::FastStr,
    }
    impl pilota::pb::Message for EchoRequest {
        #[inline]
        fn encoded_len(&self, ctx: &mut pilota::pb::EncodeLengthContext) -> usize {
            pilota::pb::encoding::faststr::encoded_len(ctx, 1, &self.message)
        }

        #[allow(unused_variables)]
        fn encode_raw(&self, buf: &mut pilota::LinkedBytes) {
            pilota::pb::encoding::faststr::encode(1, &self.message, buf);
        }

        #[allow(unused_variables)]
        fn merge_field(
            &mut self,
            tag: u32,
            wire_type: pilota::pb::encoding::WireType,
            buf: &mut pilota::Bytes,
            ctx: &mut pilota::pb::encoding::DecodeContext,
            _is_root: bool,
        ) -> core::result::Result<(), pilota::pb::DecodeError> {
            const STRUCT_NAME: &str = stringify!(EchoRequest);

            match tag {
                1 => {
                    let mut _inner_pilota_value = &mut self.message;
                    pilota::pb::encoding::faststr::merge(wire_type, _inner_pilota_value, buf, ctx)
                        .map_err(|mut error| {
                            error.push(STRUCT_NAME, stringify!(message));
                            error
                        })
                }
                _ => pilota::pb::encoding::skip_field(wire_type, tag, buf, ctx),
            }
        }
    }

    #[cfg(test)]
    #[derive(Debug)]
    struct FailingMessage(EchoRequest);

    #[cfg(test)]
    impl pilota::pb::Message for FailingMessage {
        fn encoded_len(&self, ctx: &mut pilota::pb::EncodeLengthContext) -> usize {
            self.0.encoded_len(ctx)
        }

        fn encode_raw(&self, buf: &mut pilota::LinkedBytes) {
            self.0.encode_raw(buf);
        }

        fn merge_field(
            &mut self,
            tag: u32,
            wire_type: pilota::pb::encoding::WireType,
            buf: &mut pilota::Bytes,
            ctx: &mut pilota::pb::DecodeContext,
            is_root: bool,
        ) -> Result<(), pilota::pb::DecodeError> {
            self.0.merge_field(tag, wire_type, buf, ctx, is_root)
        }

        fn encoded_len_length_delimited(
            &self,
            _ctx: &mut pilota::pb::EncodeLengthContext,
        ) -> (usize, usize) {
            (0, usize::MAX)
        }

        fn encode(&self, buf: &mut pilota::LinkedBytes) -> Result<(), pilota::pb::EncodeError> {
            if self.0.message == "fail" {
                buf.insert(pilota::Bytes::from_static(b"partial"));
                // EncodeError has no public constructor; use the trait's capacity check.
                return self.encode_length_delimited(&mut Default::default(), buf);
            }
            self.0.encode(buf)
        }
    }

    #[tokio::test]
    async fn test_encode_stream() {
        use futures::TryStreamExt;

        use super::*;
        use crate::{
            RecvStream,
            body::{Body, boxed},
            codec::decode::Kind,
        };

        let messages = [
            "Volo".into(),
            "x".repeat(8 * 1024).into(),
            "middle".into(),
            "y".repeat(64 * 1024).into(),
            "end".into(),
        ]
        .map(|message| EchoRequest { message });
        for compression in [
            None,
            #[cfg(feature = "gzip")]
            Some(CompressionEncoding::Gzip(Some(Default::default()))),
            #[cfg(feature = "zlib")]
            Some(CompressionEncoding::Zlib(Some(Default::default()))),
            #[cfg(feature = "zstd")]
            Some(CompressionEncoding::Zstd(Some(Default::default()))),
        ] {
            let source = futures::stream::iter(messages.clone().into_iter().map(Ok));
            let body = boxed(Body::new(encode(source, compression)));
            let decoded = RecvStream::<EchoRequest>::new(
                body,
                Kind::Response(http::StatusCode::OK),
                compression,
            )
            .try_collect::<Vec<_>>()
            .await
            .expect("messages decode successfully");
            assert_eq!(decoded, messages);
        }
    }

    #[tokio::test]
    async fn test_encode_pending() {
        use futures::FutureExt;

        use super::*;
        use crate::{
            RecvStream,
            body::{Body, boxed},
            codec::decode::Kind,
        };

        let source = async_stream::stream! {
            yield Ok(EchoRequest { message: "Volo".into() });
            futures::future::pending::<()>().await;
        };
        let mut stream = RecvStream::<EchoRequest>::new(
            boxed(Body::new(encode(source, None))),
            Kind::Response(http::StatusCode::OK),
            None,
        );
        let message = stream
            .next()
            .now_or_never()
            .expect("a completed message is not held behind a pending source")
            .expect("message is present")
            .expect("message decodes successfully");
        assert_eq!(message.message, "Volo");
        assert!(stream.next().now_or_never().is_none());
    }

    #[tokio::test]
    async fn test_encode_error() {
        use bytes::BytesMut;

        use super::*;
        use crate::Code;

        for source_error in [true, false] {
            let failure = if source_error {
                Err(Status::data_loss("broken"))
            } else {
                Ok(FailingMessage(EchoRequest {
                    message: "fail".into(),
                }))
            };
            let source = futures::stream::iter([
                Ok(FailingMessage(EchoRequest {
                    message: "Volo".into(),
                })),
                failure,
                Ok(FailingMessage(EchoRequest {
                    message: "tail".into(),
                })),
            ]);
            let mut stream = encode(source, None);
            let mut bytes = BytesMut::new();
            let error = loop {
                match stream.next().await.expect("stream reports its error") {
                    Ok(frame) => bytes.extend_from_slice(frame.data_ref().expect("data frame")),
                    Err(error) => break error,
                }
            };
            assert_eq!(&bytes[..], b"\x00\x00\x00\x00\x06\x0a\x04Volo");
            if source_error {
                assert_eq!(error.code(), Code::DataLoss);
                bytes.clear();
                while let Some(frame) = stream.next().await {
                    let frame = frame.expect("source continues after its error");
                    bytes.extend_from_slice(frame.data_ref().expect("data frame"));
                }
                assert_eq!(&bytes[..], b"\x00\x00\x00\x00\x06\x0a\x04tail");
            } else {
                assert_eq!(error.code(), Code::Internal);
                assert!(stream.next().await.is_none());
            }
        }
    }

    #[tokio::test]
    async fn test_encode() {
        use super::*;

        for compression in [None, Some(CompressionEncoding::Identity)] {
            let source = async_stream::stream! {
                yield Ok(EchoRequest { message: "Volo".into() });
            };

            let mut stream = encode(source, compression);
            let frame = stream
                .next()
                .await
                .expect("encoded frame is present")
                .expect("message encodes successfully");
            let data = frame.data_ref().expect("encoded frame contains data");
            assert_eq!(&data[..PREFIX_LEN], b"\x00\x00\x00\x00\x06");
            assert_eq!(&data[PREFIX_LEN..], b"\x0a\x04Volo");

            assert!(stream.next().await.is_none());
        }
    }

    #[cfg(feature = "gzip")]
    #[tokio::test]
    async fn test_encode_gzip() {
        use bytes::BytesMut;

        use super::*;
        use crate::codec::compression::{GzipConfig, decompress};

        let source = async_stream::stream! {
            yield Ok(EchoRequest { message: "Volo".into() });
        };

        let compression_encoding = CompressionEncoding::Gzip(Some(GzipConfig::default()));
        let mut stream = encode(source, Some(compression_encoding));

        // frame
        let frame = stream.next().await.unwrap().unwrap();
        assert!(frame.is_data());
        let data = frame.data_ref().unwrap();
        assert_eq!(&data[..PREFIX_LEN], b"\x01\x00\x00\x00\x1a");

        let mut compressed_data = BytesMut::from(&data[PREFIX_LEN..]);
        let mut uncompressed_data_mut = BytesMut::new();
        decompress(
            compression_encoding,
            &mut compressed_data,
            &mut uncompressed_data_mut,
        )
        .unwrap();
        assert_eq!(&uncompressed_data_mut[..], b"\x0a\x04Volo");

        assert!(stream.next().await.is_none());
    }

    #[cfg(feature = "zlib")]
    #[tokio::test]
    async fn test_encode_zlib() {
        use bytes::BytesMut;

        use super::*;
        use crate::codec::compression::{ZlibConfig, decompress};

        let source = async_stream::stream! {
            yield Ok(EchoRequest { message: "Volo".into() });
        };

        let compression_encoding = CompressionEncoding::Zlib(Some(ZlibConfig::default()));
        let mut stream = encode(source, Some(compression_encoding));

        // frame
        let frame = stream.next().await.unwrap().unwrap();
        assert!(frame.is_data());
        let data = frame.data_ref().unwrap();
        assert_eq!(&data[..PREFIX_LEN], b"\x01\x00\x00\x00\x0e");

        let mut compressed_data = BytesMut::from(&data[PREFIX_LEN..]);
        let mut uncompressed_data_mut = BytesMut::new();
        decompress(
            compression_encoding,
            &mut compressed_data,
            &mut uncompressed_data_mut,
        )
        .unwrap();
        assert_eq!(&uncompressed_data_mut[..], b"\x0a\x04Volo");

        assert!(stream.next().await.is_none());
    }

    #[cfg(feature = "zstd")]
    #[tokio::test]
    async fn test_encode_zstd() {
        use bytes::BytesMut;

        use super::*;
        use crate::codec::compression::{ZstdConfig, decompress};

        let source = async_stream::stream! {
            yield Ok(EchoRequest { message: "Volo".into() });
        };

        let compression_encoding = CompressionEncoding::Zstd(Some(ZstdConfig::default()));
        let mut stream = encode(source, Some(compression_encoding));

        // frame
        let frame = stream.next().await.unwrap().unwrap();
        assert!(frame.is_data());
        let data = frame.data_ref().unwrap();
        assert_eq!(&data[..PREFIX_LEN], b"\x01\x00\x00\x00\x0f");

        let mut compressed_data = BytesMut::from(&data[PREFIX_LEN..]);
        let mut uncompressed_data_mut = BytesMut::new();
        decompress(
            compression_encoding,
            &mut compressed_data,
            &mut uncompressed_data_mut,
        )
        .unwrap();
        assert_eq!(&uncompressed_data_mut[..], b"\x0a\x04Volo");

        assert!(stream.next().await.is_none());
    }
}
