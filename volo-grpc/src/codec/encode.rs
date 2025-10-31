use bytes::{BufMut, Bytes};
use futures::{Stream, StreamExt};
use http_body::Frame;
use linkedbytes::Node;
use pilota::{LinkedBytes, pb::Message};

use super::{DefaultEncoder, PREFIX_LEN};
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
    Box::pin(async_stream::stream! {
        futures_util::pin_mut!(source);

        loop {
            match source.next().await {
                Some(Ok(item)) => {
                    let mut buf = LinkedBytes::with_capacity(BUFFER_SIZE);
                    let mut compressed_buf = if compression_encoding.is_some() {
                        LinkedBytes::with_capacity(BUFFER_SIZE)
                    } else {
                        LinkedBytes::new()
                    };

                    buf.reserve(PREFIX_LEN);
                    unsafe {
                        buf.advance_mut(PREFIX_LEN);
                    }

                    let mut encoder=DefaultEncoder::default();

                    if let Some(config)=compression_encoding{
                        encoder.encode(item, &mut compressed_buf)
                            .map_err(|err| Status::internal(format!("Error encoding: {err}")))?;
                        compress(config,&mut compressed_buf.concat(), buf.bytes_mut())
                            .map_err(|err| Status::internal(format!("Error compressing: {err}")))?;
                    } else {
                        buf.reserve(item.encoded_len());
                        encoder.encode(item, &mut buf)
                            .map_err(|err| Status::internal(format!("Error encoding: {err}")))?;
                    }

                    let len = buf.len() - PREFIX_LEN;
                    assert!(len <= u32::MAX as usize);
                    {
                        if let Some(node) = buf.get_list_mut(0) {
                            match node {
                                linkedbytes::Node::BytesMut(bytes_mut) => {
                                    let start = bytes_mut.len() - PREFIX_LEN;
                                    let mut dest = &mut bytes_mut[start..];
                                    dest.put_u8(compression_encoding.is_some() as u8);
                                    dest.put_u32(len as u32);
                                }
                                _ => unreachable!("reserve_node_idx is not a bytesmut"),
                            };
                        } else {
                            let mut dest = &mut buf.bytes_mut()[..PREFIX_LEN];
                            dest.put_u8(compression_encoding.is_some() as u8);
                            dest.put_u32(len as u32);
                        }
                    }

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
                Some(Err(status)) => yield Err(status),
                None => break,
            }
        }
    })
}

pub mod tests {

    use super::*;

    #[derive(Debug, Default, Clone, PartialEq)]
    pub struct EchoRequest {
        pub message: ::pilota::FastStr,
    }
    impl ::pilota::pb::Message for EchoRequest {
        #[inline]
        fn encoded_len(&self) -> usize {
            0 + ::pilota::pb::encoding::faststr::encoded_len(1, &self.message)
        }

        #[allow(unused_variables)]
        fn encode_raw(&self, buf: &mut ::pilota::LinkedBytes) {
            ::pilota::pb::encoding::faststr::encode(1, &self.message, buf);
        }

        #[allow(unused_variables)]
        fn merge_field(
            &mut self,
            tag: u32,
            wire_type: ::pilota::pb::encoding::WireType,
            buf: &mut ::pilota::Bytes,
            ctx: &mut ::pilota::pb::encoding::DecodeContext,
        ) -> ::core::result::Result<(), ::pilota::pb::DecodeError> {
            const STRUCT_NAME: &'static str = stringify!(EchoRequest);

            match tag {
                1 => {
                    let mut _inner_pilota_value = &mut self.message;
                    ::pilota::pb::encoding::faststr::merge(wire_type, _inner_pilota_value, buf, ctx)
                        .map_err(|mut error| {
                            error.push(STRUCT_NAME, stringify!(message));
                            error
                        })
                }
                _ => ::pilota::pb::encoding::skip_field(wire_type, tag, buf, ctx),
            }
        }
    }

    #[tokio::test]
    async fn test_encode() {
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
