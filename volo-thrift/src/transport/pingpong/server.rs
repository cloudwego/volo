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
    tracing::SpanProvider,
    DummyMessage, EntryMessage, Error, ThriftMessage,
};

pub async fn serve<Svc, Req, Resp, E, D, SP>(
    mut encoder: E,
    mut decoder: D,
    notified: Notified<'_>,
    exit_mark: Arc<std::sync::atomic::AtomicBool>,
    service: &Svc,
    stat_tracer: Arc<[crate::server::TraceFn]>,
    peer_addr: Option<Address>,
    span_provider: SP,
) where
    Svc: Service<ServerContext, Req, Response = Resp>,
    Svc::Error: Into<Error>,
    Req: EntryMessage,
    Resp: EntryMessage,
    E: Encoder,
    D: Decoder,
    SP: SpanProvider,
{
    tokio::pin!(notified);

    metainfo::METAINFO
        .scope(RefCell::new(MetaInfo::default()), async {
            loop {
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

                let result = async {
                    let msg = tokio::select! {
                        _ = &mut notified => {
                            tracing::trace!("[VOLO] close conn by notified, peer_addr: {:?}", peer_addr);
                            return Err(());
                        },
                        out = async {
                            let msg = decoder.decode(&mut cx).await;
                            span_provider.leave_decode(&cx);
                            msg
                        }.instrument(span_provider.on_decode()) => out
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
                            let resp = service.call(&mut cx, req).await;
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
                                    span_provider.leave_encode(&cx);
                                    result
                                }.instrument(span_provider.on_encode()).await {
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

                    metainfo::METAINFO.with(|mi| {
                        mi.borrow_mut().clear();
                    });

                    span_provider.leave_serve(&cx);
                    SERVER_CONTEXT_CACHE.with(|cache| {
                        let mut cache = cache.borrow_mut();
                        if cache.len() < cache.capacity() {
                            cx.reset(Default::default());
                            cache.push(cx);
                        }
                    });
                    Ok(())
                }.instrument(span_provider.on_serve()).await;
                if let Err(_) = result {
                    break;
                }
            }
        }).await;
}
