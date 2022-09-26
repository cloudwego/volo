#![allow(unused_must_use)]
use std::{
    cell::RefCell,
    sync::{atomic::Ordering, Arc},
};

use metainfo::MetaInfo;
use motore::service::Service;
use tokio::sync::futures::Notified;
use tracing::*;
use volo::volo_unreachable;

use crate::{
    codec::{framed::Framed, Decoder, Encoder},
    context::ServerContext,
    protocol::TMessageType,
    DummyMessage, EntryMessage, Error, Size, ThriftMessage,
};

pub async fn serve<Svc, Req, Resp, E, D>(
    mut framed: Framed<E, D>,
    notified: Notified<'_>,
    exit_mark: Arc<std::sync::atomic::AtomicBool>,
    mut service: Svc,
) where
    Svc: Service<ServerContext, Req, Response = Resp>,
    Svc::Error: Into<crate::Error>,
    Req: EntryMessage,
    Resp: EntryMessage + Size,
    E: Encoder + Send,
    D: Decoder + Send,
{
    tokio::pin!(notified);

    metainfo::METAINFO
        .scope(RefCell::new(MetaInfo::default()), async {
            loop {
                // new context
                let mut cx = ServerContext::default();

                let msg = tokio::select! {
                    _ = &mut notified => {
                        tracing::trace!("[VOLO] close conn by notified");
                        return
                    },
                    out = framed.next(&mut cx) => out
                };

                tracing::debug!(
                    "[VOLO] received message: {:?}",
                    msg.as_ref().map(|msg| msg.as_ref().map(|msg| &msg.meta))
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
                            if let Err(e) = framed.send(&mut cx, msg).await {
                                // log it
                                error!("[VOLO] server send response error: {:?}", e,);
                                return;
                            }
                        }
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
                            if let Err(e) = framed.send(&mut cx, msg).await {
                                error!("[VOLO] server send error error: {:?}", e);
                            }
                        }
                        return;
                    }
                }

                ::metainfo::METAINFO.with(|mi| {
                    mi.borrow_mut().clear();
                })
            }
        })
        .await;
}
