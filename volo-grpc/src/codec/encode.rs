use bytes::{BufMut, Bytes, BytesMut};
use futures::{Stream, StreamExt};
use prost::Message;

use super::{DefaultEncoder, PREFIX_LEN};
use crate::{
    codec::{Encoder, BUFFER_SIZE},
    BoxStream, Status,
};

pub fn encode<T, S>(source: S) -> BoxStream<'static, Result<Bytes, Status>>
where
    S: Stream<Item = Result<T, Status>> + Send + 'static,
    T: Message + 'static,
{
    Box::pin(async_stream::stream! {
        let mut buf = BytesMut::with_capacity(BUFFER_SIZE);

        futures_util::pin_mut!(source);

        loop {
            match source.next().await {
                Some(Ok(item)) => {
                    buf.reserve(PREFIX_LEN);
                    unsafe {
                        buf.advance_mut(PREFIX_LEN);
                    }
                    DefaultEncoder::default().encode(item, &mut buf).map_err(|err| Status::internal(format!("Error encoding: {}", err)))?;
                    let len = buf.len() - PREFIX_LEN;
                    assert!(len <= std::u32::MAX as usize);
                    {
                        let mut buf = &mut buf[..PREFIX_LEN];
                        buf.put_u8(0);
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
