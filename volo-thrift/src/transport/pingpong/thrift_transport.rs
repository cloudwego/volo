use std::sync::atomic::AtomicUsize;

use pin_project::pin_project;
use volo::{
    net::conn::{Conn, OwnedReadHalf, OwnedWriteHalf},
    util::buf_reader::BufReader,
};

use crate::{
    codec::{Decoder, Encoder, DEFAULT_BUFFER_SIZE},
    context::{ClientContext, ThriftContext},
    transport::pool::Poolable,
    ApplicationError, ApplicationErrorKind, EntryMessage, Error, Size, ThriftMessage,
};

lazy_static::lazy_static! {
    static ref TRANSPORT_ID_COUNTER: AtomicUsize = AtomicUsize::new(0);
}

#[pin_project]
pub struct ThriftTransport<E, D> {
    read_half: ReadHalf<D>,
    write_half: WriteHalf<E>,
}

impl<E, D> ThriftTransport<E, D> {
    pub fn new(conn: Conn, encoder: E, decoder: D) -> Self {
        let (rh, wh) = conn.stream.into_split();
        let id = TRANSPORT_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            read_half: ReadHalf {
                read_half: BufReader::with_capacity(DEFAULT_BUFFER_SIZE, rh),
                decoder,
                id,
                reusable: true,
            },
            write_half: WriteHalf {
                write_half: wh,
                encoder,
                id,
                reusable: true,
            },
        }
    }

    pub fn split(self) -> (ReadHalf<D>, WriteHalf<E>) {
        (self.read_half, self.write_half)
    }
}

impl<E, D> ThriftTransport<E, D>
where
    E: Encoder,
    D: Decoder,
{
    pub async fn send<Req: EntryMessage + Size, Resp: EntryMessage>(
        &mut self,
        cx: &mut ClientContext,
        msg: ThriftMessage<Req>,
        oneway: bool,
    ) -> Result<Option<ThriftMessage<Resp>>, Error> {
        self.write_half.send(cx, msg).await?;
        if oneway {
            return Ok(None);
        }
        self.read_half.try_next(cx).await
    }
}

pub struct ReadHalf<D> {
    read_half: BufReader<OwnedReadHalf>,
    decoder: D,
    reusable: bool,
    id: usize,
}

impl<D> ReadHalf<D>
where
    D: Decoder,
{
    pub async fn try_next<T: EntryMessage>(
        &mut self,
        cx: &mut ClientContext,
    ) -> Result<Option<ThriftMessage<T>>, Error> {
        let thrift_msg = self
            .decoder
            .decode(cx, &mut self.read_half)
            .await
            .map_err(|e| {
                tracing::error!("[VOLO] transport[{}] decode error: {}", self.id, e);
                e
            })?;

        if let Some(ThriftMessage { meta, .. }) = &thrift_msg {
            if meta.seq_id != cx.seq_id {
                tracing::error!(
                    "[VOLO] transport[{}] seq_id not match: {} != {}",
                    self.id,
                    meta.seq_id,
                    cx.seq_id,
                );
                return Err(Error::Application(ApplicationError::new(
                    ApplicationErrorKind::BadSequenceId,
                    "seq_id not match",
                )));
            }
        };
        Ok(thrift_msg)
    }
}

pub struct WriteHalf<E> {
    write_half: OwnedWriteHalf,
    encoder: E,
    reusable: bool,
    id: usize,
}

impl<E> WriteHalf<E>
where
    E: Encoder,
{
    pub async fn send<T: EntryMessage + Size>(
        &mut self,
        cx: &mut impl ThriftContext,
        msg: ThriftMessage<T>,
    ) -> Result<(), Error> {
        self.encoder
            .encode(cx, &mut self.write_half, msg)
            .await
            .map_err(|e| {
                tracing::error!("[VOLO] transport[{}] encode error: {:?}", self.id, e);
                e
            })?;

        Ok(())
    }
}

impl<TTEncoder, TTDecoder> Poolable for ThriftTransport<TTEncoder, TTDecoder> {
    fn reusable(&self) -> bool {
        self.read_half.reusable && self.write_half.reusable
    }
}
