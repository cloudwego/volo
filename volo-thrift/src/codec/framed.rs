use tokio::io::AsyncWriteExt;
use volo::{
    net::conn::{OwnedReadHalf, OwnedWriteHalf},
    util::buf_reader::BufReader,
};

use super::{Decoder, Encoder, DEFAULT_BUFFER_SIZE};
use crate::{context::ThriftContext, EntryMessage, ThriftMessage};

pub struct Framed<E, D> {
    encoder: E,
    decoder: D,
    read_half: BufReader<OwnedReadHalf>,
    write_half: OwnedWriteHalf,
}

impl<E, D> Framed<E, D>
where
    E: Encoder,
    D: Decoder,
{
    pub fn new(conn: volo::net::conn::ConnStream, encoder: E, decoder: D) -> Self {
        let (read_half, write_half) = conn.into_split();
        let read_half = BufReader::with_capacity(DEFAULT_BUFFER_SIZE, read_half);
        Self {
            encoder,
            decoder,
            read_half,
            write_half,
        }
    }

    #[inline]
    pub async fn next<M: EntryMessage + Send, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
    ) -> Result<Option<ThriftMessage<M>>, crate::Error> {
        self.decoder
            .decode::<M, _, _>(cx, &mut self.read_half)
            .await
    }

    #[inline]
    pub async fn send<M: EntryMessage + crate::Size, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        msg: ThriftMessage<M>,
    ) -> Result<(), crate::Error> {
        self.encoder.encode(cx, &mut self.write_half, msg).await?;
        self.write_half.flush().await?;
        Ok(())
    }
}
