use bytes::{BufMut, Bytes, BytesMut};
use flate2::Compression;
use futures::{Stream, StreamExt};
use prost::Message;

use super::{DefaultEncoder, PREFIX_LEN};
use crate::{
    codec::{
        compression::{compress, CompressionConfig},
        Encoder, BUFFER_SIZE,
    },
    BoxStream, Status,
};

pub fn encode<T, S>(
    source: S,
    compression_config: Option<CompressionConfig>,
) -> BoxStream<'static, Result<Bytes, Status>>
where
    S: Stream<Item = Result<T, Status>> + Send + 'static,
    T: Message + 'static,
{
    Box::pin(async_stream::stream! {
        let mut buf = BytesMut::with_capacity(BUFFER_SIZE);

        futures_util::pin_mut!(source);

        let (mut compressed_buf,level)= if compression_config.is_some() {
            (BytesMut::with_capacity(BUFFER_SIZE),Compression::new(compression_config.unwrap().level))
        } else {
           (BytesMut::new(),Compression::none())
        };

        loop {
            match source.next().await {
                Some(Ok(item)) => {
                    buf.reserve(PREFIX_LEN);
                    unsafe {
                        buf.advance_mut(PREFIX_LEN);
                    }
                    let mut encoder=DefaultEncoder::default();

                    if let Some(config)=compression_config{
                        compressed_buf.clear();
                        encoder.encode(item, &mut compressed_buf)
                            .map_err(|err| Status::internal(format!("Error encoding: {}", err)))?;
                        compress(config.encoding,&mut compressed_buf,&mut buf, level)
                            .map_err(|err| Status::internal(format!("Error compressing: {}", err)))?;
                    }else{
                        encoder.encode(item, &mut buf)
                            .map_err(|err| Status::internal(format!("Error encoding: {}", err)))?;
                    }
                    let len = buf.len() - PREFIX_LEN;
                    assert!(len <= std::u32::MAX as usize);
                    {
                        let mut buf = &mut buf[..PREFIX_LEN];
                        buf.put_u8(compression_config.is_some() as u8);
                        buf.put_u32(len as u32);
                    }

                    yield Ok(buf.split_to(len + PREFIX_LEN).freeze());
                },
                Some(Err(status)) => yield Err(status),
                None => break,
            }
        }
    })
}
