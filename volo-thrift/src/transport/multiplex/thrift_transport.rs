use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicBool, AtomicUsize},
        Arc,
    },
};

use metainfo::MetaInfo;
use pin_project::pin_project;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{oneshot, Mutex},
};
use volo::{
    context::{Role, RpcInfo},
    net::Address,
};

use crate::{
    codec::{Decoder, Encoder, MakeCodec},
    context::{ClientContext, ThriftContext},
    transport::pool::{Poolable, Reservation},
    ApplicationError, ApplicationErrorKind, EntryMessage, Error, ThriftMessage,
};

lazy_static::lazy_static! {
    // This is used for debug.
    static ref TRANSPORT_ID_COUNTER: AtomicUsize = AtomicUsize::new(0);
}

#[pin_project]
pub struct ThriftTransport<E, Resp> {
    write_half: Arc<Mutex<WriteHalf<E>>>,
    #[allow(clippy::type_complexity)]
    tx_map: Arc<
        Mutex<
            fxhash::FxHashMap<
                i32,
                oneshot::Sender<
                    crate::Result<Option<(MetaInfo, ClientContext, ThriftMessage<Resp>)>>,
                >,
            >,
        >,
    >,
    write_error: Arc<AtomicBool>,
    // read has error
    read_error: Arc<AtomicBool>,
    // read connection is closed
    read_closed: Arc<AtomicBool>,
}

impl<E, Resp> Clone for ThriftTransport<E, Resp> {
    fn clone(&self) -> Self {
        Self {
            write_half: self.write_half.clone(),
            tx_map: self.tx_map.clone(),
            write_error: self.write_error.clone(),
            read_error: self.read_error.clone(),
            read_closed: self.read_closed.clone(),
        }
    }
}

impl<E, Resp> ThriftTransport<E, Resp>
where
    E: Encoder,
{
    pub fn new<
        R: AsyncRead + Send + Sync + Unpin + 'static,
        W: AsyncWrite + Send + Sync + Unpin + 'static,
        MkC: MakeCodec<R, W, Encoder = E>,
    >(
        read_half: R,
        write_half: W,
        make_codec: MkC,
        target: Address,
    ) -> Self
    where
        Resp: EntryMessage + Send + 'static,
    {
        tracing::trace!(
            "[VOLO] creating multiplex thrift transport, target: {}",
            target
        );
        let id = TRANSPORT_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (encoder, decoder) = make_codec.make_codec(read_half, write_half);
        let mut read_half = ReadHalf { decoder, id };
        let write_half = WriteHalf { encoder, id };
        #[allow(clippy::type_complexity)]
        let tx_map: Arc<
            Mutex<
                fxhash::FxHashMap<
                    i32,
                    oneshot::Sender<
                        crate::Result<Option<(MetaInfo, ClientContext, ThriftMessage<Resp>)>>,
                    >,
                >,
            >,
        > = Default::default();
        let inner_tx_map = tx_map.clone();
        let write_error = Arc::new(AtomicBool::new(false));
        let inner_write_error = write_error.clone();
        let read_error = Arc::new(AtomicBool::new(false));
        let inner_read_error = read_error.clone();
        let read_closed = Arc::new(AtomicBool::new(false));
        let inner_read_closed = read_closed.clone();
        tokio::spawn(async move {
            metainfo::METAINFO
                .scope(RefCell::new(Default::default()), async move {
                    loop {
                        if inner_write_error.load(std::sync::atomic::Ordering::Relaxed) {
                            tracing::trace!(
                                "[VOLO] multiplex write error, break read loop now, target: {}",
                                target
                            );
                            break;
                        }
                        // fake context
                        let mut cx = ClientContext::new(
                            -1,
                            RpcInfo::with_role(Role::Client),
                            pilota::thrift::TMessageType::Call,
                        );
                        let res = read_half.try_next::<Resp>(&mut cx, target.clone()).await;
                        if let Err(e) = res {
                            tracing::error!(
                                "[VOLO] multiplex connection read error: {}, target: {}",
                                e,
                                target
                            );
                            let mut tx_map = inner_tx_map.lock().await;
                            inner_read_error.store(true, std::sync::atomic::Ordering::Relaxed);
                            for (_, tx) in tx_map.drain() {
                                let _ = tx.send(Err(Error::Application(ApplicationError::new(
                                    ApplicationErrorKind::Unknown,
                                    format!("multiplex connection error: {e}, target: {target}"),
                                ))));
                            }
                            return;
                        }
                        // we have checked the error above, so it's safe to unwrap here
                        let res = res.unwrap();
                        if res.is_none() {
                            // the connection is closed
                            let mut tx_map = inner_tx_map.lock().await;
                            if !tx_map.is_empty() {
                                inner_read_error.store(true, std::sync::atomic::Ordering::Relaxed);
                                for (_, tx) in tx_map.drain() {
                                    let _ = tx.send(Ok(None));
                                }
                            }
                            inner_read_closed.store(true, std::sync::atomic::Ordering::Relaxed);
                            return;
                        }
                        // now we get ThriftMessage<Resp>
                        let res = res.unwrap();
                        let seq_id = res.meta.seq_id;
                        let mut tx_map = inner_tx_map.lock().await;
                        if let Some(tx) = tx_map.remove(&seq_id) {
                            metainfo::METAINFO.with(|mi| {
                                let mi = mi.take();
                                let _ = tx.send(Ok(Some((mi, cx, res))));
                            });
                        } else {
                            tracing::error!(
                                "[VOLO] multiplex connection receive unexpected response, seq_id: \
                                 {}, target: {}",
                                seq_id,
                                target
                            );
                        }
                    }
                })
                .await;
        });
        Self {
            write_half: Arc::new(Mutex::new(write_half)),
            tx_map,
            write_error,
            read_error,
            read_closed,
        }
    }
}

impl<E, Resp> ThriftTransport<E, Resp>
where
    E: Encoder,
    Resp: EntryMessage,
{
    pub async fn send<Req: EntryMessage>(
        &self,
        cx: &mut ClientContext,
        msg: ThriftMessage<Req>,
        oneway: bool,
    ) -> Result<Option<ThriftMessage<Resp>>, Error> {
        let (tx, rx) = oneshot::channel();
        let mut tx_map = self.tx_map.lock().await;
        // check error and closed
        if self.read_error.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(Error::Application(ApplicationError::new(
                ApplicationErrorKind::Unknown,
                "multiplex connection error".to_string(),
            )));
        }
        if self.read_closed.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(Error::Application(ApplicationError::new(
                ApplicationErrorKind::Unknown,
                "multiplex connection closed".to_string(),
            )));
        }
        let seq_id = msg.meta.seq_id;
        if !oneway {
            tx_map.insert(seq_id, tx);
        }
        drop(tx_map);
        if let Err(e) = self.write_half.lock().await.send(cx, msg).await {
            self.write_error
                .store(true, std::sync::atomic::Ordering::Relaxed);
            if !oneway {
                let mut tx_map = self.tx_map.lock().await;
                tx_map.remove(&seq_id);
            }
            return Err(e);
        }
        if oneway {
            return Ok(None);
        }
        match rx.await {
            Ok(res) => match res {
                Ok(opt) => match opt {
                    None => Ok(None),
                    Some((mi, new_cx, msg)) => {
                        metainfo::METAINFO.with(|m| {
                            m.borrow_mut().extend(mi);
                        });
                        // TODO: cx extend
                        if let Some(t) = new_cx.common_stats.decode_start_at() {
                            cx.common_stats.set_decode_start_at(t);
                        }
                        if let Some(t) = new_cx.common_stats.decode_end_at() {
                            cx.common_stats.set_decode_end_at(t);
                        }
                        if let Some(t) = new_cx.common_stats.read_start_at() {
                            cx.common_stats.set_read_start_at(t);
                        }
                        if let Some(t) = new_cx.common_stats.read_end_at() {
                            cx.common_stats.set_read_end_at(t);
                        }
                        if let Some(s) = new_cx.common_stats.read_size() {
                            cx.common_stats.set_read_size(s);
                        }
                        Ok(Some(msg))
                    }
                },
                Err(e) => Err(e),
            },
            Err(e) => {
                tracing::error!("[VOLO] multiplex connection oneshot recv error: {e}");
                Err(Error::Application(ApplicationError::new(
                    ApplicationErrorKind::Unknown,
                    format!("multiplex connection oneshot recv error: {e}"),
                )))
            }
        }
    }
}

pub struct ReadHalf<D> {
    decoder: D,
    id: usize,
}

impl<D> ReadHalf<D>
where
    D: Decoder,
{
    pub async fn try_next<T: EntryMessage>(
        &mut self,
        cx: &mut ClientContext,
        target: Address,
    ) -> Result<Option<ThriftMessage<T>>, Error> {
        let thrift_msg = self.decoder.decode(cx).await.map_err(|e| {
            tracing::error!(
                "[VOLO] transport[{}] decode error: {}, target: {}",
                self.id,
                e,
                target
            );
            e
        })?;

        // TODO: move this to recv
        // if let Some(ThriftMessage { meta, .. }) = &thrift_msg {
        //     if meta.seq_id != cx.seq_id {
        //         tracing::error!(
        //             "[VOLO] transport[{}] seq_id not match: {} != {}",
        //             self.id,
        //             meta.seq_id,
        //             cx.seq_id,
        //         );
        //         return Err(Error::Application(ApplicationError::new(
        //             ApplicationErrorKind::BadSequenceId,
        //             "seq_id not match",
        //         )));
        //     }
        // };
        Ok(thrift_msg)
    }
}

pub struct WriteHalf<E> {
    encoder: E,
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
        self.encoder.encode(cx, msg).await.map_err(|mut e| {
            e.append_msg(&format!(", rpcinfo: {:?}", cx.rpc_info()));
            tracing::error!("[VOLO] transport[{}] encode error: {:?}", self.id, e);
            e
        })?;

        Ok(())
    }
}

impl<TTEncoder, Resp> Poolable for ThriftTransport<TTEncoder, Resp> {
    fn reusable(&self) -> bool {
        !self.write_error.load(std::sync::atomic::Ordering::Relaxed)
            && !self.read_error.load(std::sync::atomic::Ordering::Relaxed)
            && !self.read_closed.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn reserve(self) -> Reservation<Self> {
        Reservation::Shared(self.clone(), self)
    }

    fn can_share(&self) -> bool {
        true
    }
}
