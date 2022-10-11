use tokio::io::AsyncWriteExt;
use volo::{
    net::conn::{OwnedReadHalf, OwnedWriteHalf},
    util::buf_reader::BufReader,
};

use super::{Decoder, Encoder, DEFAULT_BUFFER_SIZE};
use crate::{context::ThriftContext, EntryMessage, ThriftMessage};

pub struct ReadHalf<D> {
    decoder: D,
    read_half: BufReader<OwnedReadHalf>,
}

impl<D> ReadHalf<D>
where
    D: Decoder + Send,
{
    #[inline]
    pub async fn next<M: EntryMessage + Send, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
    ) -> Result<Option<ThriftMessage<M>>, crate::Error> {
        self.decoder
            .decode::<M, _, _>(cx, &mut self.read_half)
            .await
    }
}

pub struct WriteHalf<E> {
    encoder: E,
    write_half: OwnedWriteHalf,
}

impl<E> WriteHalf<E>
where
    E: Encoder + Send,
{
    #[inline]
    pub async fn send<M: EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        msg: ThriftMessage<M>,
    ) -> Result<(), crate::Error> {
        self.encoder.encode(cx, &mut self.write_half, msg).await?;
        self.write_half.flush().await?;
        Ok(())
    }
}

pub struct Framed<E, D> {
    read_half: ReadHalf<D>,
    write_half: WriteHalf<E>,
}

impl<E, D> Framed<E, D>
where
    E: Encoder + Send,
    D: Decoder + Send,
{
    pub fn new(conn: volo::net::conn::ConnStream, encoder: E, decoder: D) -> Self {
        let (read_half, write_half) = conn.into_split();
        let read_half = BufReader::with_capacity(DEFAULT_BUFFER_SIZE, read_half);
        let write_half = WriteHalf {
            encoder,
            write_half,
        };
        let read_half = ReadHalf { decoder, read_half };
        Self {
            read_half,
            write_half,
        }
    }

    #[inline]
    pub async fn next<M: EntryMessage + Send, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
    ) -> Result<Option<ThriftMessage<M>>, crate::Error> {
        self.read_half.next(cx).await
    }

    #[inline]
    pub async fn send<M: EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        msg: ThriftMessage<M>,
    ) -> Result<(), crate::Error> {
        self.write_half.send(cx, msg).await
    }

    pub fn into_split(self) -> (ReadHalf<D>, WriteHalf<E>) {
        (self.read_half, self.write_half)
    }
}
