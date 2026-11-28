use std::{
    cell::RefCell,
    future::Future,
    sync::{Arc, atomic::Ordering},
};

use metainfo::MetaInfo;
use motore::service::Service;
use pilota::thrift::ThriftException;
use tokio::sync::{mpsc, oneshot};
use tracing::*;
use volo::{context::Context, net::Address, volo_unreachable};

use crate::{
    DummyMessage, EntryMessage, ServerError, ThriftMessage,
    codec::{Decoder, Encoder},
    context::{ServerContext, ThriftContext as _},
    protocol::TMessageType,
    server_error_to_application_exception, thrift_exception_to_application_exception,
    transport::should_log,
};

const CHANNEL_SIZE: usize = 1024;

type QueuedResponse<Resp> = (
    MetaInfo,
    ServerContext,
    ThriftMessage<Resp>,
    Option<oneshot::Sender<()>>,
);

pub async fn serve<Svc, Req, Resp, E, D>(
    encoder: E,
    decoder: D,
    notified: impl Future<Output = ()>,
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
    serve_with_mode(
        encoder,
        decoder,
        notified,
        exit_mark,
        service,
        stat_tracer,
        peer_addr,
        false,
        Default::default(),
    )
    .await;
}

/// The mixed entry keeps Buffered serial while retaining native framed multiplexing.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve_with_mode<Svc, Req, Resp, E, D>(
    mut encoder: E,
    mut decoder: D,
    notified: impl Future<Output = ()>,
    exit_mark: Arc<std::sync::atomic::AtomicBool>,
    service: Svc,
    stat_tracer: Arc<[crate::server::TraceFn]>,
    peer_addr: Option<Address>,
    serialize_buffered: bool,
    unsafe_codec: Option<bool>,
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
    let (send_tx, mut send_rx) = mpsc::channel::<QueuedResponse<Resp>>(CHANNEL_SIZE);
    let (error_send_tx, mut error_send_rx) =
        mpsc::channel::<(ServerContext, ThriftMessage<DummyMessage>)>(1);

    let writer = tokio::spawn({
        let peer_addr = peer_addr.clone();
        async move {
            metainfo::METAINFO
                .scope(RefCell::new(MetaInfo::default()), async {
                    let mut errors_open = true;
                    loop {
                        tokio::select! {
                            // receives a response, we need to send it back to client
                            msg = send_rx.recv() => {
                                match msg {
                                    Some((mi, mut cx, msg, written)) => {
                                        if let Err(e) = metainfo::METAINFO
                                            .scope(
                                                RefCell::new(mi),
                                                encoder.encode::<Resp, ServerContext>(&mut cx, msg),
                                            )
                                            .await
                                        {
                                            stat_tracer.iter().for_each(|f| f(&cx));
                                            if should_log(&e) {
                                                // log it
                                                error!(
                                                    "[VOLO] server send response error: {:?}, cx: \
                                                    {:?}, peer_addr: {:?}",
                                                    e, cx, peer_addr
                                                );
                                            }
                                            return;
                                        }
                                        stat_tracer.iter().for_each(|f| f(&cx));
                                        if let Some(written) = written {
                                            let _ = written.send(());
                                        }
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
                            error_msg = error_send_rx.recv(), if errors_open => {
                                match error_msg {
                                    Some((mut cx, msg)) => {
                                        cx.set_conn_reset_by_ttheader(true);
                                        if let Err(e) = encoder
                                            .encode::<DummyMessage, ServerContext>(&mut cx, msg)
                                            .await
                                        {
                                            stat_tracer.iter().for_each(|f| f(&cx));
                                            if should_log(&e) {
                                                // log it
                                                error!(
                                                    "[VOLO] server send error error: {:?}, cx: {:?}, \
                                                    peer_addr: {:?}",
                                                    e, cx, peer_addr
                                                );
                                            }
                                            return;
                                        }
                                        stat_tracer.iter().for_each(|f| f(&cx));
                                        return;
                                    }
                                    None => {
                                        // EOF ends admission; accepted handlers still own send_tx.
                                        errors_open = false;
                                    }
                                }
                            }
                        }
                    }
                })
                .await;
        }
    });

    let mut tasks = ManagedTasks::new(writer, serialize_buffered);
    metainfo::METAINFO
        .scope(RefCell::new(MetaInfo::default()), async {
            loop {
                // Reap completed calls between messages. Do not race join_next
                // with decode: canceling a partially read message loses bytes.
                while tasks.native.handlers.try_join_next().is_some() {}
                // new context
                let mut cx = ServerContext::default();
                cx.rpc_info.config_mut().set_unsafe_codec(unsafe_codec);
                if let Some(peer_addr) = &peer_addr {
                    cx.rpc_info_mut()
                        .caller_mut()
                        .set_address(peer_addr.clone());
                }

                tokio::select! {
                    biased;
                    _ = &mut notified => {
                        tracing::trace!(
                            "[VOLO] close conn by notified, peer_addr: {:?}",
                            peer_addr
                        );
                        return;
                    }
                    // receives a message
                    msg = decoder.decode(&mut cx) => {
                        #[cfg(feature = "shmipc")]
                        decoder.shmipc_helper().release_read_and_reuse();
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
                                if should_log(&e) {
                                    error!(
                                        "[VOLO] multiplex server decode error {:?}, peer_addr: {:?}",
                                        e, peer_addr
                                    );
                                }
                                cx.msg_type = Some(TMessageType::Exception);
                                if !matches!(e, ThriftException::Transport(_))
                                    && cx.req_msg_type != Some(TMessageType::OneWay) {
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

                        let serial = serialize_buffered
                            && cx.unframed;
                        #[cfg(feature = "shmipc")]
                        if serialize_buffered && !serial && decoder.shmipc_helper().available() {
                            tracing::error!("multiplex is not supported when using shmipc");
                            return;
                        }
                        let (written_tx, written_rx) = if serial && cx.req_msg_type != Some(TMessageType::OneWay) {
                            let (tx, rx) = oneshot::channel();
                            (Some(tx), Some(rx))
                        } else { (None, None) };

                        // Keep the native response path; Buffered awaits its write acknowledgement.
                        let svc = service.clone();
                        let exit_mark = exit_mark.clone();
                        let send_tx = send_tx.clone();
                        let mi = metainfo::METAINFO.with(|m| m.take());
                        let process = volo::streaming::ServerMiddleware::scope_optional(volo::streaming::ServerMiddleware::current(), async move {
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
                                        let _ = send_tx.send((mi, cx, msg, written_tx)).await;
                                    }
                                })
                                .await;
                        });
                        let owner = crate::streaming::unary::ConnectionOwner::current();
                        let process = async move {
                            if let Some(owner) = owner { owner.scope(process).await; }
                            else { process.await; }
                        };
                        if serial {
                            process.await;
                            if let Some(written) = written_rx {
                                if written.await.is_err() { return; }
                            }
                        } else {
                            if serialize_buffered { tasks.native.handlers.spawn(process); }
                            else { tokio::spawn(process); }
                        }
                    }
                }
            }
        })
        .await;
    if serialize_buffered {
        while tasks.native.handlers.join_next().await.is_some() {}
        drop(send_tx);
        drop(error_send_tx);
        let _ = tasks.native.writer.as_mut().expect("native writer").await;
    }
}

struct ManagedTasks {
    native: crate::streaming::unary::MultiplexTasks,
    managed: bool,
    owner: Option<Arc<crate::streaming::unary::ConnectionOwner>>,
}
impl ManagedTasks {
    fn new(writer: tokio::task::JoinHandle<()>, managed: bool) -> Self {
        Self {
            native: crate::streaming::unary::MultiplexTasks {
                handlers: tokio::task::JoinSet::new(),
                writer: Some(writer),
            },
            managed,
            owner: managed
                .then(crate::streaming::unary::ConnectionOwner::current)
                .flatten(),
        }
    }
}
impl Drop for ManagedTasks {
    fn drop(&mut self) {
        if !self.managed {
            return;
        }
        let writer = self.native.writer.as_ref().expect("native writer");
        if self.native.handlers.is_empty() && writer.is_finished() {
            return;
        }
        self.native.handlers.abort_all();
        writer.abort();
        if let Some(owner) = &self.owner {
            // Abort only schedules task destruction. Preserve the actual native
            // handles so connection reclamation can await that destruction.
            owner.reclaim_multiplex(std::mem::take(&mut self.native));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use volo::streaming::NativeConnection;

    use super::*;

    struct Dropped(Arc<AtomicUsize>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Release);
        }
    }

    async fn held(drops: Arc<AtomicUsize>, ready: oneshot::Sender<()>) {
        let _guard = Dropped(drops);
        let _ = ready.send(());
        std::future::pending::<()>().await;
    }

    #[tokio::test]
    async fn forced_connection_drop_waits_for_native_handler_and_writer_destructors() {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            for reclaim_before_drop in [false, true] {
                for abandon_first_waiter in [false, true] {
                    let owner = Arc::new(crate::streaming::unary::ConnectionOwner::default());
                    let drops = Arc::new(AtomicUsize::new(0));
                    let (ready, entered) = oneshot::channel();
                    let mut connection = Box::pin({
                        let owner = owner.clone();
                        let drops = drops.clone();
                        async move {
                            let _guard = crate::streaming::unary::ConnectionGuard(owner.clone());
                            owner
                                .scope(async {
                                    let (writer_ready, writer_entered) = oneshot::channel();
                                    let mut tasks = ManagedTasks::new(
                                        tokio::spawn(held(drops.clone(), writer_ready)),
                                        true,
                                    );
                                    writer_entered.await.unwrap();
                                    for _ in 0..2 {
                                        let (ready, entered) = oneshot::channel();
                                        tasks.native.handlers.spawn(held(drops.clone(), ready));
                                        entered.await.unwrap();
                                    }
                                    let _ = ready.send(());
                                    std::future::pending::<()>().await;
                                })
                                .await;
                        }
                    });
                    tokio::select! {
                        _ = &mut connection => unreachable!("connection must remain pending"),
                        entered = entered => entered.unwrap(),
                    }
                    // Work before typed RPC admission must also be joined, even
                    // though the connection's call-context map is still empty.
                    let mut reclaimed = owner.reclaimed();
                    if reclaim_before_drop {
                        assert!(futures::poll!(&mut reclaimed).is_pending());
                    }
                    drop(connection);
                    assert_eq!(drops.load(Ordering::Acquire), 0);
                    assert!(futures::poll!(&mut reclaimed).is_pending());
                    if abandon_first_waiter {
                        drop(reclaimed);
                        reclaimed = owner.reclaimed();
                        assert!(futures::poll!(&mut reclaimed).is_pending());
                    }
                    reclaimed.await;
                    assert_eq!(drops.load(Ordering::Acquire), 3);
                    owner.reclaimed().await;
                }
            }
        })
        .await
        .unwrap();
    }
}
