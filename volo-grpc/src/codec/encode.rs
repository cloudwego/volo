use bytes::{BufMut, Bytes};
use futures::{Stream, StreamExt};
use http_body::Frame;
use pilota::{pb::Message, LinkedBytes};

use super::{DefaultEncoder, PREFIX_LEN};
use crate::{
    codec::{
        compression::{compress, CompressionEncoding},
        Encoder, BUFFER_SIZE,
    },
    BoxStream, Status,
};

pub fn encode<T, S>(
    source: S,
    compression_encoding: Option<CompressionEncoding>,
) -> BoxStream<'static, Result<Frame<Bytes>, Status>>
where
    S: Stream<Item = Result<T, Status>> + Send + Sync + 'static,
    T: Message + 'static,
{
    Box::pin(async_stream::stream! {
        let mut buf = LinkedBytes::with_capacity(BUFFER_SIZE);
        let mut compressed_buf= if compression_encoding.is_some() {
            LinkedBytes::with_capacity(BUFFER_SIZE)
        } else {
           LinkedBytes::new()
        };

        futures_util::pin_mut!(source);

        loop {
            match source.next().await {
                Some(Ok(item)) => {
                    buf.reserve(PREFIX_LEN);
                    unsafe {
                        buf.advance_mut(PREFIX_LEN);
                    }
                    let mut encoder=DefaultEncoder::default();

                    if let Some(config)=compression_encoding{
                        compressed_buf.reset();
                        encoder.encode(item, &mut compressed_buf)
                            .map_err(|err| Status::internal(format!("Error encoding: {err}")))?;
                        compress(config,&mut compressed_buf,&mut buf)
                            .map_err(|err| Status::internal(format!("Error compressing: {err}")))?;
                    }else{
                        encoder.encode(item, &mut buf)
                            .map_err(|err| Status::internal(format!("Error encoding: {err}")))?;
                    }
                    let len = buf.bytes().len() - PREFIX_LEN;
                    assert!(len <= u32::MAX as usize);
                    {
                        let mut buf = &mut buf.bytes_mut()[..PREFIX_LEN];
                        buf.put_u8(compression_encoding.is_some() as u8);
                        buf.put_u32(len as u32);
                    }

                    yield Ok(Frame::data(buf.bytes_mut().split_to(len + PREFIX_LEN).freeze()));
                },
                Some(Err(status)) => yield Err(status),
                None => break,
            }
        }
    })
}
