#![allow(unused_must_use)]
use std::{
    cell::RefCell,
    sync::{atomic::Ordering, Arc},
};

use metainfo::MetaInfo;
use motore::service::Service;
use tokio::sync::{futures::Notified, mpsc};
use tracing::*;
use volo::{net::Address, volo_unreachable};

use crate::{
    codec::{Decoder, Encoder},
    context::ServerContext,
    protocol::TMessageType,
    DummyMessage, EntryMessage, Error, ThriftMessage,
};

const CHANNEL_SIZE: usize = 1024;

pub async fn serve<Svc, Req, Resp, E, D>(
    mut encoder: E,
    mut decoder: D,
    notified: Notified<'_>,
    exit_mark: Arc<std::sync::atomic::AtomicBool>,
    service: Svc,
    peer_addr: Option<Address>,
) where
    Svc: Service<ServerContext, Req, Response = Resp> + Send + Clone + 'static + Sync,
    Svc::Error: Into<Error> + Send,
    Req: EntryMessage + 'static,
    Resp: EntryMessage + 'static,
    E: Encoder,
    D: Decoder,
{
    tokio::pin!(notified);

    // mpsc channel used to send responses to the loop
    let (send_tx, mut send_rx) = mpsc::channel(CHANNEL_SIZE);
    let (error_send_tx, mut error_send_rx) = mpsc::channel(1);

    tokio::spawn({
        let peer_addr = peer_addr.clone();
        async move {
            metainfo::METAINFO
            .scope(RefCell::new(MetaInfo::default()), async {
                loop {
                    tokio::select! {
                        // receives a response, we need to send it back to client
                        msg = send_rx.recv() => {
                            match msg {
                                Some((mi, mut cx, msg)) => {
                                    if let Err(e) = metainfo::METAINFO.scope(RefCell::new(mi), encoder.encode::<Resp, ServerContext>(&mut cx, msg)).await {
                                        // log it
                                        error!("[VOLO] server send response error: {:?}, rpcinfo: {:?}, peer_addr: {:?}", e, cx.rpc_info, peer_addr);
                                        return;
                                    }
                                }
                                None => {
                                    // log it
                                    trace!("[VOLO] server send channel closed, peer_addr: {:?}", peer_addr);
                                    return;
                                }
                            }
                        },
                        // receives an error, we need to close the connection
                        error_msg = error_send_rx.recv() => {
                            match error_msg {
                                Some((mut cx, msg)) => {
                                    if let Err(e) = encoder.encode::<DummyMessage, ServerContext>(&mut cx, msg).await {
                                        // log it
                                        error!("[VOLO] server send error error: {:?}, rpcinfo: {:?}, peer_addr: {:?}", e, cx.rpc_info, peer_addr);
                                    }
                                    return;
                                }
                                None => {
                                    // log it
                                    trace!("[VOLO] server send error channel closed, peer_addr: {:?}", peer_addr);
                                    return;
                                }
                            }
                        }
                    }
                }
            })
            .await;
        }
    });

    metainfo::METAINFO
        .scope(RefCell::new(MetaInfo::default()), async {
            loop {
                // new context
                let mut cx = ServerContext::default();

                tokio::select! {
                    _ = &mut notified => {
                        tracing::trace!("[VOLO] close conn by notified, peer_addr: {:?}", peer_addr);
                        return
                    },
                    // receives a message
                    msg = decoder.decode(&mut cx) => {
                        tracing::debug!(
                            "[VOLO] received message: {:?}, rpcinfo: {:?}, peer_addr: {:?}",
                            msg.as_ref().map(|msg| msg.as_ref().map(|msg| &msg.meta)),
                            cx.rpc_info,
                            peer_addr
                        );
                        match msg {
                            Ok(Some(ThriftMessage { data: Ok(req), .. })) => {
                                // if it's ok, then we need to spawn this msg to a new task
                                let svc = service.clone();
                                let exit_mark = exit_mark.clone();
                                let send_tx = send_tx.clone();
                                let mi = metainfo::METAINFO.with(|m| m.take());
                                tokio::spawn(async  {
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
                                trace!("[VOLO] reach eof, connection has been closed by client, peer_addr: {:?}", peer_addr);
                                return;
                            }
                            Err(e) => {
                                error!("[VOLO] multiplex server decode error {:?}, peer_addr: {:?}", e, peer_addr);
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
