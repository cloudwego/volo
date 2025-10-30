use bytes::{BufMut, Bytes};
use futures::{Stream, StreamExt};
use http_body::Frame;
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
            let mut buf = LinkedBytes::with_capacity(BUFFER_SIZE);
            let mut compressed_buf = if compression_encoding.is_some() {
                LinkedBytes::with_capacity(BUFFER_SIZE)
            } else {
                LinkedBytes::new()
            };
            match source.next().await {
                Some(Ok(item)) => {
                    let reserve_node_idx = {
                        buf.reserve(PREFIX_LEN);
                        unsafe {
                            buf.advance_mut(PREFIX_LEN);
                        }
                        buf.split()
                    };

                    let mut encoder=DefaultEncoder::default();

                    if let Some(config)=compression_encoding{
                        compressed_buf.reset();
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
                        match buf.get_list_mut(reserve_node_idx).expect("reserve_node_idx is valid") {
                            linkedbytes::Node::BytesMut(bytes_mut) => {
                                let start = bytes_mut.len() - PREFIX_LEN;
                                let mut buf = &mut bytes_mut[start..];
                                buf.put_u8(compression_encoding.is_some() as u8);
                                buf.put_u32(len as u32);
                            }
                            _ => unreachable!("reserve_node_idx is not a bytesmut"),
                        }
                    }

                    // remove the trailing empty bytes
                    yield Ok(Frame::data(buf.concat().split_to(len + PREFIX_LEN).freeze()));
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

    #[cfg(feature = "gzip")]
    #[tokio::test]
    async fn test_encode_gzip() {
        use bytes::BytesMut;

        use crate::codec::compression::{GzipConfig, decompress};

        let source = async_stream::stream! {
            yield Ok(EchoRequest { message: "Volo".into() });
        };

        let compression_encoding = Some(CompressionEncoding::Gzip(Some(GzipConfig::default())));
        let result = encode(source, compression_encoding).next().await.unwrap();

        assert!(result.is_ok());
        let frame = result.unwrap();
        assert!(frame.is_data());
        let data = frame.data_ref().unwrap();
        let mut data_mut = BytesMut::from(&data[PREFIX_LEN..]);
        let mut uncompressed_data_mut = BytesMut::new();
        decompress(
            compression_encoding.unwrap(),
            &mut data_mut,
            &mut uncompressed_data_mut,
        )
        .unwrap();
        assert_eq!(&uncompressed_data_mut[..], b"\x0a\x04Volo");
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
        let result = encode(source, compression_encoding).next().await.unwrap();

        assert!(result.is_ok());
        let frame = result.unwrap();
        assert!(frame.is_data());
        let data = frame.data_ref().unwrap();
        let mut data_mut = BytesMut::from(&data[PREFIX_LEN..]);
        let mut uncompressed_data_mut = BytesMut::new();
        decompress(
            compression_encoding.unwrap(),
            &mut data_mut,
            &mut uncompressed_data_mut,
        )
        .unwrap();
        assert_eq!(&uncompressed_data_mut[..], b"\x0a\x04Volo");
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
        let result = encode(source, compression_encoding).next().await.unwrap();

        assert!(result.is_ok());
        let frame = result.unwrap();
        assert!(frame.is_data());
        let data = frame.data_ref().unwrap();
        let mut data_mut = BytesMut::from(&data[PREFIX_LEN..]);
        let mut uncompressed_data_mut = BytesMut::new();
        decompress(
            compression_encoding.unwrap(),
            &mut data_mut,
            &mut uncompressed_data_mut,
        )
        .unwrap();
        assert_eq!(&uncompressed_data_mut[..], b"\x0a\x04Volo");
    }
}
