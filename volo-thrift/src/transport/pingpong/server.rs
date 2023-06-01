#![allow(unused_must_use)]
use std::{
    cell::RefCell,
    sync::{atomic::Ordering, Arc},
};

use metainfo::MetaInfo;
use motore::service::Service;
use tokio::sync::futures::Notified;
use tracing::*;
use volo::{context::Endpoint, net::Address, volo_unreachable};

use crate::{
    codec::{Decoder, Encoder},
    context::{ServerContext, SERVER_CONTEXT_CACHE},
    protocol::TMessageType,
    tracing::{ServerField, ServerState},
    DummyMessage, EntryMessage, Error, ThriftMessage,
};

pub async fn serve<Svc, Req, Resp, E, D>(
    mut encoder: E,
    mut decoder: D,
    notified: Notified<'_>,
    exit_mark: Arc<std::sync::atomic::AtomicBool>,
    service: &Svc,
    stat_tracer: Arc<[crate::server::TraceFn]>,
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
                let result = async {
                    // new context
                    let mut cx = SERVER_CONTEXT_CACHE.with(|cache| {
                        let mut cache = cache.borrow_mut();
                        cache.pop().unwrap_or_default()
                    });
                    if let Some(peer_addr) = &peer_addr {
                        let mut caller = Endpoint::new("-".into());
                        caller.set_address(peer_addr.clone());
                        cx.rpc_info.caller = Some(caller);
                    }

                    let msg = tokio::select! {
                        _ = &mut notified => {
                            tracing::trace!("[VOLO] close conn by notified, peer_addr: {:?}", peer_addr);
                            return Err(());
                        },
                        out = async {
                            let result = decoder.decode(&mut cx).await;
                            Span::current().record(ServerField::RECV_SIZE, cx.common_stats.read_size());
                            result
                        }.instrument(span!(Level::TRACE, ServerState::DECODE)) => out
                    };

                    debug!(
                        "[VOLO] received message: {:?}, rpcinfo: {:?}, peer_addr: {:?}",
                        msg.as_ref().map(|msg| msg.as_ref().map(|msg| &msg.meta)),
                        cx.rpc_info,
                        peer_addr
                    );

                    match msg {
                        Ok(Some(ThriftMessage { data: Ok(req), .. })) => {
                            cx.stats.record_process_start_at();
                            let resp = service.call(&mut cx, req).instrument(span!(Level::TRACE, ServerState::HANDLE)).await;
                            cx.stats.record_process_end_at();

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
                                if let Err(e) = async {
                                    let result = encoder.encode(&mut cx, msg).await;
                                    Span::current().record(ServerField::SEND_SIZE, cx.common_stats.write_size());
                                    result
                                }.instrument(span!(Level::TRACE, ServerState::ENCODE)).await {
                                    // log it
                                    error!("[VOLO] server send response error: {:?}, rpcinfo: {:?}, peer_addr: {:?}", e, cx.rpc_info, peer_addr);
                                    stat_tracer.iter().for_each(|f| f(&cx));
                                    return Err(());
                                }
                            }
                        }
                        Ok(Some(ThriftMessage { data: Err(_), .. })) => {
                            volo_unreachable!();
                        }
                        Ok(None) => {
                            trace!("[VOLO] reach eof, connection has been closed by client, peer_addr: {:?}", peer_addr);
                            return Err(());
                        }
                        Err(e) => {
                            error!("[VOLO] pingpong server decode error: {:?}, peer_addr: {:?}", e, peer_addr);
                            cx.msg_type = Some(TMessageType::Exception);
                            if !matches!(e, Error::Transport(_)) {
                                let msg = ThriftMessage::mk_server_resp(&cx, Err::<DummyMessage, _>(e))
                                    .unwrap();
                                if let Err(e) = encoder.encode(&mut cx, msg).await {
                                    error!("[VOLO] server send error error: {:?}, rpcinfo: {:?}, peer_addr: {:?}", e, cx.rpc_info, peer_addr);
                                }
                            }
                            stat_tracer.iter().for_each(|f| f(&cx));
                            return Err(());
                        }
                    }
                    stat_tracer.iter().for_each(|f| f(&cx));

                    SERVER_CONTEXT_CACHE.with(|cache| {
                        let mut cache = cache.borrow_mut();
                        if cache.len() < cache.capacity() {
                            cx.reset(Default::default());
                            cache.push(cx);
                        }
                    });

                    metainfo::METAINFO.with(|mi| {
                        mi.borrow_mut().clear();
                    });
                    Ok(())
                }.instrument(span!(Level::TRACE, ServerState::SERVE)).await;
                if let Err(_) = result {
                    break;
                }
            }
        }).await;
}
