use std::sync::atomic::AtomicUsize;

use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{
    codec::{Decoder, Encoder, MakeCodec},
    context::{ClientContext, ThriftContext},
    transport::pool::Poolable,
    ApplicationError, ApplicationErrorKind, EntryMessage, Error, ThriftMessage,
};

lazy_static::lazy_static! {
    static ref TRANSPORT_ID_COUNTER: AtomicUsize = AtomicUsize::new(0);
}

#[pin_project]
pub struct ThriftTransport<E: Encoder, D: Decoder> {
    write_half: WriteHalf<E>,
    read_half: ReadHalf<D>,
}

impl<E, D> ThriftTransport<E, D>
where
    E: Encoder,
    D: Decoder,
{
    pub fn new<
        R: AsyncRead + Send + Sync + Unpin + 'static,
        W: AsyncWrite + Send + Sync + Unpin + 'static,
        MkC: MakeCodec<R, W, Decoder = D, Encoder = E>,
    >(
        read_half: R,
        write_half: W,
        make_codec: MkC,
    ) -> Self {
        let id = TRANSPORT_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (encoder, decoder) = make_codec.make_codec(read_half, write_half);
        Self {
            read_half: ReadHalf {
                decoder,
                id,
                reusable: true,
            },
            write_half: WriteHalf {
                encoder,
                id,
                reusable: true,
            },
        }
    }

    #[allow(dead_code)]
    pub fn split(self) -> (ReadHalf<D>, WriteHalf<E>) {
        (self.read_half, self.write_half)
    }
}

impl<E, D> ThriftTransport<E, D>
where
    E: Encoder,
    D: Decoder,
{
    pub async fn send<Req: EntryMessage, Resp: EntryMessage>(
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
        let thrift_msg = self.decoder.decode(cx).await.map_err(|e| {
            let mut e = e;
            e.append_msg(&format!(", cx: {:?}", cx));
            tracing::error!("[VOLO] transport[{}] decode error: {}", self.id, e);
            e
        })?;

        if let Some(ThriftMessage { meta, .. }) = &thrift_msg {
            if meta.seq_id != cx.seq_id {
                tracing::error!(
                    "[VOLO] transport[{}] seq_id not match: {} != {}, cx: {:?}",
                    self.id,
                    meta.seq_id,
                    cx.seq_id,
                    cx,
                );
                return Err(Error::Application(ApplicationError::new(
                    ApplicationErrorKind::BAD_SEQUENCE_ID,
                    format!("seq_id not match, cx: {:?}", cx),
                )));
            }
        };
        Ok(thrift_msg)
    }
}

pub struct WriteHalf<E> {
    encoder: E,
    reusable: bool,
    id: usize,
}

impl<E> WriteHalf<E>
where
    E: Encoder,
{
    pub async fn send<T: EntryMessage>(
        &mut self,
        cx: &mut impl ThriftContext,
        msg: ThriftMessage<T>,
    ) -> Result<(), Error> {
        self.encoder.send(cx, msg).await.map_err(|mut e| {
            e.append_msg(&format!(", rpcinfo: {:?}", cx.rpc_info()));
            tracing::error!("[VOLO] transport[{}] send error: {:?}", self.id, e);
            e
        })?;

        Ok(())
    }
}

impl<E, D> Poolable for ThriftTransport<E, D>
where
    E: Encoder,
    D: Decoder,
{
    fn reusable(&self) -> bool {
        self.read_half.reusable && self.write_half.reusable
    }
}
