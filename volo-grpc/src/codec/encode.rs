use bytes::{BufMut, Bytes};
use futures::{Stream, StreamExt};
use http_body::Frame;
use linkedbytes::Node;
use pilota::{LinkedBytes, pb::Message};

use super::{CodecLimits, DefaultEncoder, PREFIX_LEN};
use crate::{
    BoxStream, Status,
    codec::{
        BUFFER_SIZE, Encoder,
        compression::{CompressionEncoding, compress},
    },
};

pub fn encode<T, S>(
    source: S,
    compression_encoding: Option<CompressionEncoding>,
) -> BoxStream<'static, Result<Frame<Bytes>, Status>>
where
    S: Stream<Item = Result<T, Status>> + Send + 'static,
    T: Message + 'static,
{
    encode_with_encoder(source, compression_encoding, DefaultEncoder::<T>::default())
}

/// Encode using one codec instance for the whole stream.
pub fn encode_with_encoder<T, S, E>(
    source: S,
    compression_encoding: Option<CompressionEncoding>,
    encoder: E,
) -> BoxStream<'static, Result<Frame<Bytes>, Status>>
where
    S: Stream<Item = Result<T, Status>> + Send + 'static,
    E: Encoder<Item = T> + Send + 'static,
    E::Error: Into<Status>,
    T: Send + 'static,
{
    encode_with_options(source, compression_encoding, encoder, CodecLimits::legacy())
}

/// Encode with explicit per-message limits. Codec state is never recreated for
/// a subsequent message, including on the compressed path.
pub fn encode_with_options<T, S, E>(
    source: S,
    compression: Option<CompressionEncoding>,
    encoder: E,
    limits: CodecLimits,
) -> BoxStream<'static, Result<Frame<Bytes>, Status>>
where
    S: Stream<Item = Result<T, Status>> + Send + 'static,
    E: Encoder<Item = T> + Send + 'static,
    E::Error: Into<Status>,
    T: Send + 'static,
{
    Box::pin(encode_inner(
        source,
        compression,
        encoder,
        limits,
        Unobserved,
    ))
}

/// The context-free branch keeps a static zero-sized observer. Registered RPC
/// state is scoped around producer polls without adding a box per message.
pub fn encode_with_context<T, S, E>(
    source: S,
    compression: Option<CompressionEncoding>,
    encoder: E,
    limits: CodecLimits,
    context: Option<crate::streaming::StreamContext>,
    exception_classifier: Option<fn(&T) -> bool>,
) -> BoxStream<'static, Result<Frame<Bytes>, Status>>
where
    S: Stream<Item = Result<T, Status>> + Send + 'static,
    E: Encoder<Item = T> + Send + 'static,
    E::Error: Into<Status>,
    T: Send + 'static,
{
    match context {
        Some(context) => Box::pin(ScopedEncoding {
            inner: encode_inner(
                source,
                compression,
                encoder,
                limits,
                Observed {
                    context: context.clone(),
                    exception_classifier,
                },
            ),
            context,
        }),
        None => encode_with_options(source, compression, encoder, limits),
    }
}

trait EncodeObserver<T>: Send + 'static {
    fn before_encode(&self, _value: &T) {}
    fn on_send(&self, _size: usize) {}
}
struct Unobserved;
impl<T> EncodeObserver<T> for Unobserved {}
struct Observed<T> {
    context: crate::streaming::StreamContext,
    exception_classifier: Option<fn(&T) -> bool>,
}
impl<T: 'static> EncodeObserver<T> for Observed<T> {
    fn before_encode(&self, value: &T) {
        if self
            .exception_classifier
            .is_some_and(|classify| classify(value))
        {
            self.context.record_declared_exception();
        }
    }
    fn on_send(&self, size: usize) {
        self.context.on_send(size);
    }
}

#[pin_project::pin_project]
struct ScopedEncoding<S> {
    #[pin]
    inner: S,
    context: crate::streaming::StreamContext,
}
impl<S: Stream> Stream for ScopedEncoding<S> {
    type Item = S::Item;
    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        task: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.project();
        this.context.sync_scope(|| this.inner.poll_next(task))
    }
}

fn encode_inner<T, S, E, O: EncodeObserver<T>>(
    source: S,
    compression_encoding: Option<CompressionEncoding>,
    mut encoder: E,
    limits: CodecLimits,
    observer: O,
) -> impl Stream<Item = Result<Frame<Bytes>, Status>> + Send + 'static
where
    S: Stream<Item = Result<T, Status>> + Send + 'static,
    E: Encoder<Item = T> + Send + 'static,
    E::Error: Into<Status>,
    T: Send + 'static,
{
    let compression_encoding = compression_encoding.filter(|c| *c != CompressionEncoding::Identity);
    async_stream::stream! {
        futures_util::pin_mut!(source);

        loop {
            match source.next().await {
                Some(Ok(item)) => {
                    observer.before_encode(&item);
                    let mut buf = LinkedBytes::with_capacity(BUFFER_SIZE);
                    buf.reserve(PREFIX_LEN);
                    buf.put_slice(&[0; PREFIX_LEN]);

                    let encoded_size;
                    if let Some(config)=compression_encoding{
                        let mut payload = LinkedBytes::with_capacity(BUFFER_SIZE);
                        encoder.encode_with_limit(item, &mut payload, limits.max_send_message_size)?;
                        encoded_size = payload.len();
                        if encoded_size > limits.max_send_message_size {
                            Err(Status::resource_exhausted("encoded message exceeds size limit"))?;
                        }
                        compress(config, payload.into_iter_list(), buf.bytes_mut())
                            .map_err(|err| Status::internal(format!("Error compressing: {err}")))?;
                    } else {
                        encoder.encode_with_limit(item, &mut buf, limits.max_send_message_size)?;
                        encoded_size = buf.len() - PREFIX_LEN;
                    }

                    let len = buf.len() - PREFIX_LEN;
                    if len > limits.max_send_message_size {
                        Err(Status::resource_exhausted("encoded message exceeds size limit"))?;
                    }
                    let wire_len = u32::try_from(len)
                        .map_err(|_| Status::resource_exhausted("encoded message exceeds gRPC length range"))?;
                    {
                        if let Some(node) = buf.get_list_mut(0) {
                            match node {
                                linkedbytes::Node::BytesMut(bytes_mut) => {
                                    let mut dest = &mut bytes_mut[..PREFIX_LEN];
                                    dest.put_u8(compression_encoding.is_some() as u8);
                                    dest.put_u32(wire_len);
                                }
                                _ => unreachable!("reserve_node_idx is not a bytesmut"),
                            };
                        } else {
                            let mut dest = &mut buf.bytes_mut()[..PREFIX_LEN];
                            dest.put_u8(compression_encoding.is_some() as u8);
                            dest.put_u32(wire_len);
                        }
                    }

                    observer.on_send(encoded_size);
                    // send each node in linked bytes as a separate frame
                    for node in buf.into_iter_list() {
                        let bytes = match node {
                            Node::Bytes(bytes) => bytes,
                            Node::BytesMut(bytesmut) => bytesmut.freeze(),
                            Node::FastStr(faststr) => faststr.into_bytes(),
                        };
                        if !bytes.is_empty() {
                            yield Ok(Frame::data(bytes));
                        }
                    }
                },
                Some(Err(status)) => {
                    yield Err(status);
                    break;
                },
                None => break,
            }
        }
    }
}

pub mod tests {

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

    #[tokio::test]
    async fn test_encode() {
        use super::*;
        let source = async_stream::stream! {
            yield Ok(EchoRequest { message: "Volo".into() });
        };

        let mut stream = encode(source, None);
        // frame
        let frame = stream.next().await.unwrap().unwrap();
        assert!(frame.is_data());
        let data = frame.data_ref().unwrap();
        assert_eq!(&data[..PREFIX_LEN], b"\x00\x00\x00\x00\x06");
        assert_eq!(&data[PREFIX_LEN..], b"\x0a\x04Volo");

        assert!(stream.next().await.is_none());
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

        let compression_encoding = Some(CompressionEncoding::Gzip(Some(GzipConfig::default())));
        let mut stream = encode(source, compression_encoding);

        // frame
        let frame = stream.next().await.unwrap().unwrap();
        assert!(frame.is_data());
        let data = frame.data_ref().unwrap();
        assert_eq!(&data[..PREFIX_LEN], b"\x01\x00\x00\x00\x1a");

        let mut compressed_data = BytesMut::from(&data[PREFIX_LEN..]);
        let mut uncompressed_data_mut = BytesMut::new();
        decompress(
            compression_encoding.unwrap(),
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

        let compression_encoding = Some(CompressionEncoding::Zlib(Some(ZlibConfig::default())));
        let mut stream = encode(source, compression_encoding);

        // frame
        let frame = stream.next().await.unwrap().unwrap();
        assert!(frame.is_data());
        let data = frame.data_ref().unwrap();
        assert_eq!(&data[..PREFIX_LEN], b"\x01\x00\x00\x00\x0e");

        let mut compressed_data = BytesMut::from(&data[PREFIX_LEN..]);
        let mut uncompressed_data_mut = BytesMut::new();
        decompress(
            compression_encoding.unwrap(),
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

        let compression_encoding = Some(CompressionEncoding::Zstd(Some(ZstdConfig::default())));
        let mut stream = encode(source, compression_encoding);

        // frame
        let frame = stream.next().await.unwrap().unwrap();
        assert!(frame.is_data());
        let data = frame.data_ref().unwrap();
        assert_eq!(&data[..PREFIX_LEN], b"\x01\x00\x00\x00\x0f");

        let mut compressed_data = BytesMut::from(&data[PREFIX_LEN..]);
        let mut uncompressed_data_mut = BytesMut::new();
        decompress(
            compression_encoding.unwrap(),
            &mut compressed_data,
            &mut uncompressed_data_mut,
        )
        .unwrap();
        assert_eq!(&uncompressed_data_mut[..], b"\x0a\x04Volo");

        assert!(stream.next().await.is_none());
    }
}
