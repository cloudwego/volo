use std::{
    cell::RefCell,
    sync::{atomic::Ordering, Arc},
};

use metainfo::MetaInfo;
use motore::service::Service;
use pilota::thrift::ThriftException;
use tokio::sync::{futures::Notified, mpsc};
use tracing::*;
use volo::{context::Context, net::Address, volo_unreachable};

use crate::{
    codec::{Decoder, Encoder},
    context::{ServerContext, ThriftContext as _},
    protocol::TMessageType,
    server_error_to_application_exception, thrift_exception_to_application_exception, DummyMessage,
    EntryMessage, ServerError, ThriftMessage,
};

const CHANNEL_SIZE: usize = 1024;

pub async fn serve<Svc, Req, Resp, E, D>(
    mut encoder: E,
    mut decoder: D,
    notified: Notified<'_>,
    exit_mark: Arc<std::sync::atomic::AtomicBool>,
    service: Svc,
    stat_tracer: Arc<[crate::server::TraceFn]>,
    peer_addr: Option<Address>,
) where
    Svc: Service<ServerContext, Req, Response = Resp> + Send + Clone + 'static + Sync,
    Svc::Error: Into<ServerError> + Send,
    Req: EntryMessage + 'static,
    Resp: EntryMessage + 'static,
    E: Encoder,
    D: Decoder,
{
    tokio::pin!(notified);

    // mpsc channel used to send responses to the loop
    let (send_tx, mut send_rx) = mpsc::channel(CHANNEL_SIZE);
    let (error_send_tx, mut error_send_rx) =
        mpsc::channel::<(ServerContext, ThriftMessage<DummyMessage>)>(1);

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
                                        if let Err(e) = metainfo::METAINFO
                                            .scope(
                                                RefCell::new(mi),
                                                encoder.encode::<Resp, ServerContext>(&mut cx, msg),
                                            )
                                            .await
                                        {
                                            stat_tracer.iter().for_each(|f| f(&cx));
                                            if let ThriftException::Transport(te) = &e {
                                                if volo::util::server_remote_error::is_remote_closed_error(te.io_error())
                                                    && !volo::util::server_remote_error::remote_closed_error_log_enabled()
                                                {
                                                    return;
                                                }
                                            }
                                            // log it
                                            error!(
                                                "[VOLO] server send response error: {:?}, cx: \
                                                 {:?}, peer_addr: {:?}",
                                                e, cx, peer_addr
                                            );
                                            return;
                                        }
                                        stat_tracer.iter().for_each(|f| f(&cx));
                                        if cx.encode_conn_reset() {
                                            return;
                                        }
                                    }
                                    None => {
                                        // log it
                                        trace!(
                                            "[VOLO] server send channel closed, peer_addr: {:?}",
                                            peer_addr
                                        );
                                        return;
                                    }
                                }
                            }
                            // receives an error, we need to close the connection
                            error_msg = error_send_rx.recv() => {
                                match error_msg {
                                    Some((mut cx, msg)) => {
                                        cx.set_conn_reset_by_ttheader(true);
                                        if let Err(e) = encoder
                                            .encode::<DummyMessage, ServerContext>(&mut cx, msg)
                                            .await
                                        {
                                            stat_tracer.iter().for_each(|f| f(&cx));
                                            if let ThriftException::Transport(te) = &e {
                                                if volo::util::server_remote_error::is_remote_closed_error(te.io_error())
                                                    && !volo::util::server_remote_error::remote_closed_error_log_enabled()
                                                {
                                                    return;
                                                }
                                            }
                                            // log it
                                            error!(
                                                "[VOLO] server send error error: {:?}, cx: {:?}, \
                                                 peer_addr: {:?}",
                                                e, cx, peer_addr
                                            );
                                            return;
                                        }
                                        stat_tracer.iter().for_each(|f| f(&cx));
                                        return;
                                    }
                                    None => {
                                        // log it
                                        trace!(
                                            "[VOLO] server send error channel closed, peer_addr: \
                                             {:?}",
                                            peer_addr
                                        );
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
                if let Some(peer_addr) = &peer_addr {
                    cx.rpc_info_mut()
                        .caller_mut()
                        .set_address(peer_addr.clone());
                }

                tokio::select! {
                    _ = &mut notified => {
                        tracing::trace!(
                            "[VOLO] close conn by notified, peer_addr: {:?}",
                            peer_addr
                        );
                        return;
                    }
                    // receives a message
                    msg = decoder.decode(&mut cx) => {
                        tracing::debug!(
                            "[VOLO] received message: {:?}, cx: {:?}, peer_addr: {:?}",
                            msg.as_ref().map(|msg| msg.as_ref().map(|msg| &msg.meta)),
                            cx,
                            peer_addr
                        );
                        let req = match msg {
                            Ok(Some(ThriftMessage { data: Ok(req), .. })) => req,
                            Ok(Some(ThriftMessage { data: Err(_), .. })) => {
                                volo_unreachable!();
                            }
                            Ok(None) => {
                                trace!(
                                    "[VOLO] reach eof, connection has been closed by client, \
                                     peer_addr: {:?}",
                                    peer_addr
                                );
                                return;
                            }
                            Err(e) => {
                                error!(
                                    "[VOLO] multiplex server decode error {:?}, peer_addr: {:?}",
                                    e, peer_addr
                                );
                                cx.msg_type = Some(TMessageType::Exception);
                                if !matches!(e, ThriftException::Transport(_)) {
                                    let msg = ThriftMessage::mk_server_resp(
                                        &cx,
                                        Err::<DummyMessage, _>(
                                            thrift_exception_to_application_exception(e),
                                        ),
                                    );
                                    let _ = error_send_tx.send((cx, msg)).await;
                                }
                                return;
                            }
                        };

                        // if it's ok, then we need to spawn this msg to a new task
                        let svc = service.clone();
                        let exit_mark = exit_mark.clone();
                        let send_tx = send_tx.clone();
                        let mi = metainfo::METAINFO.with(|m| m.take());
                        tokio::spawn(async {
                            metainfo::METAINFO
                                .scope(RefCell::new(mi), async move {
                                    cx.stats.record_process_start_at();
                                    let resp = svc.call(&mut cx, req).await.map_err(Into::into);
                                    cx.stats.record_process_end_at();

                                    if exit_mark.load(Ordering::Relaxed) {
                                        cx.set_conn_reset_by_ttheader(true);
                                    }
                                    let req_msg_type =
                                        cx.req_msg_type.expect("`req_msg_type` should be set.");
                                    if req_msg_type != TMessageType::OneWay {
                                        cx.msg_type = Some(match resp {
                                            Ok(_) => TMessageType::Reply,
                                            Err(_) => TMessageType::Exception,
                                        });
                                        let msg = ThriftMessage::mk_server_resp(
                                            &cx,
                                            resp.map_err(server_error_to_application_exception),
                                        );
                                        let mi = metainfo::METAINFO.with(|m| m.take());
                                        let _ = send_tx.send((mi, cx, msg)).await;
                                    }
                                })
                                .await;
                        });
                    }
                }
            }
        })
        .await;
}
