#![allow(unused_must_use)]
use std::{
    cell::RefCell,
    sync::{atomic::Ordering, Arc},
};

use metainfo::MetaInfo;
use motore::service::Service;
use tokio::sync::futures::Notified;
use tracing::*;
use volo::{net::Address, volo_unreachable};

use crate::{
    codec::{Decoder, Encoder},
    context::ServerContext,
    protocol::TMessageType,
    DummyMessage, EntryMessage, Error, ThriftMessage,
};

pub async fn serve<Svc, Req, Resp, E, D>(
    mut encoder: E,
    mut decoder: D,
    notified: Notified<'_>,
    exit_mark: Arc<std::sync::atomic::AtomicBool>,
    service: &Svc,
    peer_addr: Option<Address>,
) where
    Svc: Service<ServerContext, Req, Response = Resp>,
    Svc::Error: Into<Error>,
    Req: EntryMessage,
    Resp: EntryMessage,
    E: Encoder,
    D: Decoder,
{
    tokio::pin!(notified);

    metainfo::METAINFO
        .scope(RefCell::new(MetaInfo::default()), async {
            loop {
                // new context
                let mut cx = ServerContext::default();

                let msg = tokio::select! {
                    _ = &mut notified => {
                        tracing::trace!("[VOLO] close conn by notified, peer_addr: {:?}", peer_addr);
                        return
                    },
                    out = decoder.decode(&mut cx) => out
                };

                debug!(
                    "[VOLO] received message: {:?}, rpcinfo: {:?}, peer_addr: {:?}",
                    msg.as_ref().map(|msg| msg.as_ref().map(|msg| &msg.meta)),
                    cx.rpc_info,
                    peer_addr
                );

                match msg {
                    Ok(Some(ThriftMessage { data: Ok(req), .. })) => {
                        let resp = service.call(&mut cx, req).await;

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
                            if let Err(e) = encoder.encode(&mut cx, msg).await {
                                // log it
                                error!("[VOLO] server send response error: {:?}, rpcinfo: {:?}, peer_addr: {:?}", e, cx.rpc_info, peer_addr);
                                return;
                            }
                        }
                    }
                    Ok(Some(ThriftMessage { data: Err(_), .. })) => {
                        volo_unreachable!();
                    }
                    Ok(None) => {
                        trace!("[VOLO] reach eof, connection has been closed by client, peer_addr: {:?}", peer_addr);
                        return;
                    }
                    Err(e) => {
                        error!("[VOLO] pingpong server decode error: {:?}, peer_addr: {:?}", e, peer_addr);
                        cx.msg_type = Some(TMessageType::Exception);
                        if !matches!(e, Error::Pilota(pilota::thrift::error::Error::Transport(_))) {
                            let msg = ThriftMessage::mk_server_resp(&cx, Err::<DummyMessage, _>(e))
                                .unwrap();
                            if let Err(e) = encoder.encode(&mut cx, msg).await {
                                error!("[VOLO] server send error error: {:?}, rpcinfo: {:?}, peer_addr: {:?}", e, cx.rpc_info, peer_addr);
                            }
                        }
                        return;
                    }
                }

                metainfo::METAINFO.with(|mi| {
                    mi.borrow_mut().clear();
                })
            }
        })
        .await;
}
