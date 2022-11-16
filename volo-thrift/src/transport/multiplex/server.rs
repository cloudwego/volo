#![allow(unused_must_use)]
use std::{
    cell::RefCell,
    sync::{atomic::Ordering, Arc},
};

use metainfo::MetaInfo;
use motore::service::Service;
use tokio::sync::{futures::Notified, mpsc};
use tracing::*;
use volo::volo_unreachable;

use crate::{
    codec::{framed::Framed, Decoder, Encoder},
    context::ServerContext,
    protocol::TMessageType,
    DummyMessage, EntryMessage, Error, ThriftMessage,
};

const CHANNEL_SIZE: usize = 1024;

pub async fn serve<Svc, Req, Resp, E, D>(
    framed: Framed<E, D>,
    notified: Notified<'_>,
    exit_mark: Arc<std::sync::atomic::AtomicBool>,
    service: Svc,
) where
    Svc: Service<ServerContext, Req, Response = Resp> + Send + Clone + 'static,
    Svc::Error: Into<Error> + Send,
    Req: EntryMessage + 'static,
    Resp: EntryMessage + 'static,
    E: Encoder + Send + 'static,
    D: Decoder + Send + 'static,
{
    tokio::pin!(notified);

    let (mut read_half, mut write_half) = framed.into_split();

    // mpsc channel used to send responses to the loop
    let (send_tx, mut send_rx) = mpsc::channel(CHANNEL_SIZE);
    let (error_send_tx, mut error_send_rx) = mpsc::channel(1);

    tokio::spawn(async move {
        metainfo::METAINFO
            .scope(RefCell::new(MetaInfo::default()), async {
                loop {
                    tokio::select! {
                        // receives a response, we need to send it back to client
                        msg = send_rx.recv() => {
                            match msg {
                                Some((mi, mut cx, msg)) => {
                                    if let Err(e) = metainfo::METAINFO.scope(RefCell::new(mi), write_half.send(&mut cx, msg)).await {
                                        // log it
                                        error!("[VOLO] server send response error: {:?}", e,);
                                        return;
                                    }
                                }
                                None => {
                                    // log it
                                    info!("[VOLO] server send channel closed");
                                    return;
                                }
                            }
                        },
                        // receives an error, we need to close the connection
                        error_msg = error_send_rx.recv() => {
                            match error_msg {
                                Some((mut cx, msg)) => {
                                    if let Err(e) = write_half.send(&mut cx, msg).await {
                                        // log it
                                        error!("[VOLO] server send error error: {:?}", e,);
                                    }
                                    return;
                                }
                                None => {
                                    // log it
                                    info!("[VOLO] server send error channel closed");
                                    return;
                                }
                            }
                        }
                    }
                }
            })
            .await;
    });

    metainfo::METAINFO
        .scope(RefCell::new(MetaInfo::default()), async {
            loop {
                // new context
                let mut cx = ServerContext::default();

                tokio::select! {
                    _ = &mut notified => {
                        tracing::trace!("[VOLO] close conn by notified");
                        return
                    },
                    // receives a message
                    msg = read_half.next(&mut cx) => {
                        tracing::debug!(
                            "[VOLO] received message: {:?}",
                            msg.as_ref().map(|msg| msg.as_ref().map(|msg| &msg.meta))
                        );
                        match msg {
                            Ok(Some(ThriftMessage { data: Ok(req), .. })) => {
                                // if it's ok, then we need to spawn this msg to a new task
                                let mut svc = service.clone();
                                let exit_mark = exit_mark.clone();
                                let send_tx = send_tx.clone();
                                let mi = metainfo::METAINFO.with(|m| m.take());
                                tokio::spawn(async move {
                                    metainfo::METAINFO.scope(RefCell::new(mi), async move {
                                        let resp = svc.call(&mut cx, req).await;

                                        if exit_mark.load(Ordering::Relaxed) {
                                            cx.transport.set_conn_reset(true);
                                        }
                                        if cx.req_msg_type.unwrap() != TMessageType::OneWay {
                                            cx.msg_type = Some(match resp {
                                                Ok(_) => TMessageType::Reply,
                                                Err(_) => TMessageType::Exception,
                                            });
                                            let msg =
                                                ThriftMessage::mk_server_resp(&cx, resp.map_err(|e| e.into()))
                                                    .unwrap();
                                            let mi = metainfo::METAINFO.with(|m| m.take());
                                            send_tx.send((mi, cx, msg)).await;
                                        }
                                    }).await;
                                });
                            }
                            Ok(Some(ThriftMessage { data: Err(_), .. })) => {
                                volo_unreachable!();
                            }
                            Ok(None) => {
                                trace!("[VOLO] reach eof, connection has been closed by client");
                                return;
                            }
                            Err(e) => {
                                error!("{:?}", e);
                                cx.msg_type = Some(TMessageType::Exception);
                                if !matches!(e, Error::Pilota(pilota::thrift::error::Error::Transport(_))) {
                                    let msg = ThriftMessage::mk_server_resp(&cx, Err::<DummyMessage, _>(e))
                                        .unwrap();
                                    error_send_tx.send((cx, msg)).await;
                                }
                                return;
                            }
                        }
                    }
                }
            }
        })
        .await;
}
