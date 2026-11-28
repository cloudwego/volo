//! Push production belongs to the native RPC; its result is not wire completion.

use std::{future::Future, panic::AssertUnwindSafe};

use futures::FutureExt;
use tokio::task::AbortHandle;

use super::{Inner, StreamContext};
use crate::{Code, Status};

#[derive(Default)]
pub(super) struct Production {
    result: Option<Status>,
    ready: bool,
    handler: Option<AbortHandle>,
    running: bool,
    pub(super) backward: super::metadata::ResponseMetainfo,
}

impl Inner {
    pub(super) fn producer_error(&self) -> Option<Status> {
        if let Some(status) = self.terminal.get() {
            return Some(if status.status.code() == Code::Ok {
                Status::cancelled("RPC finished")
            } else {
                status.status.clone()
            });
        }
        self.production
            .lock()
            .as_ref()
            .and_then(|p| p.result.clone())
            .map(|status| {
                if status.code() == Code::Ok {
                    Status::cancelled("response handler finished")
                } else {
                    status
                }
            })
            .or_else(|| {
                self.token
                    .is_cancelled()
                    .then(|| self.cancellation_status())
            })
    }

    pub(super) fn cancellation_status(&self) -> Status {
        if self.deadline.is_some_and(|deadline| deadline.is_expired()) {
            Status::deadline_exceeded("timeout")
        } else {
            Status::cancelled("parent RPC finished")
        }
    }
}

impl StreamContext {
    pub(super) fn response_handler_reclaimed(&self) -> bool {
        self.0.production.lock().as_ref().is_none_or(|p| !p.running)
    }
    pub(crate) fn attach_h2_owner(&self) {
        self.0
            .h2_owned
            .store(true, std::sync::atomic::Ordering::Release);
    }
    pub(crate) fn defers_response_commit(&self) -> bool {
        self.0.h2_owned.load(std::sync::atomic::Ordering::Acquire) && self.has_production()
    }
    pub(crate) fn commit_response(
        &self,
        trailers: http::HeaderMap,
        send: impl FnOnce(http::HeaderMap) -> Result<(), Status>,
    ) -> Result<(), Status> {
        let metadata = crate::metadata::MetadataMap::from_headers(trailers.clone());
        let mut status = Status::from_header_map(&trailers)
            .unwrap_or_else(|| Status::internal("missing final grpc-status"));
        let (result, won) = {
            let production = self.0.production.lock();
            if let Some(terminal) = self.status() {
                return Err(terminal.status.clone());
            }
            if self.0.token.is_cancelled() {
                return Err(self.cancellation_status());
            }
            if let Some(original) = production.as_ref().and_then(|p| p.result.as_ref()) {
                status.retain_source(original);
            }
            let result = send(trailers);
            let terminal = result.as_ref().err().cloned().unwrap_or(status);
            let won = self.record_terminal(terminal);
            (result, won)
        };
        if won {
            if result.is_ok() {
                self.on_trailer(metadata);
            }
            self.finish_cleanup();
        }
        result
    }
    pub(crate) fn production_changed(&self) -> tokio::sync::futures::Notified<'_> {
        self.0.production_changed.notified()
    }
    pub(crate) fn has_production(&self) -> bool {
        self.0.production.lock().is_some()
    }
    pub(crate) fn production_result(&self) -> Option<Status> {
        self.0
            .production
            .lock()
            .as_ref()
            .and_then(|p| p.result.clone())
    }
    pub(crate) fn producer_error(&self) -> Option<Status> {
        self.0.producer_error()
    }

    pub(crate) async fn producer_ended(&self) -> Status {
        loop {
            let changed = self.0.production_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(status) = self.producer_error() {
                return status;
            }
            tokio::select! {
                _ = changed => {},
                status = self.cancelled() => return status,
            }
        }
    }

    pub(crate) fn commit_produced<R>(&self, enqueue: impl FnOnce() -> R) -> Result<R, Status> {
        let mut slot = self.0.production.lock();
        if let Some(status) = self.status() {
            return Err(if status.status.code() == Code::Ok {
                self.cancellation_status()
            } else {
                status.status.clone()
            });
        }
        if self.0.token.is_cancelled() {
            return Err(self.cancellation_status());
        }
        let state = slot
            .as_mut()
            .ok_or_else(|| Status::failed_precondition("response producer was not started"))?;
        if state.result.is_some() {
            return Err(Status::cancelled("response handler finished"));
        }
        if !state.ready {
            self.with_metainfo(|info| state.backward.publish(info));
        }
        let result = enqueue();
        state.ready = true;
        drop(slot);
        self.0.production_changed.notify_waiters();
        Ok(result)
    }

    pub(crate) async fn response_ready(&self) -> Result<(), Status> {
        loop {
            let changed = self.0.production_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.0.production.lock().as_ref().is_some_and(|p| p.ready) {
                return Ok(());
            }
            tokio::select! {
                _ = changed => {},
                status = self.cancelled() => return Err(status),
            }
        }
    }

    pub(crate) fn start_response_handler<F>(&self, handler: F) -> Result<(), Status>
    where
        F: Future<Output = Result<(), Status>> + Send + 'static,
    {
        let owner = self.own_response_handler()?;
        let cx = self.clone();
        let (start, started) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(self.0.metainfo.task_scope(async move {
            let _owner = owner;
            if started.await.is_err() {
                return;
            }
            let result = AssertUnwindSafe(cx.shared_context().scope(handler))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err(Status::internal("response handler panicked")));
            let status = match result {
                Ok(()) => Status::new(Code::Ok, ""),
                Err(status) if status.code() == Code::Ok => {
                    Status::internal("response handler returned OK as an error")
                }
                Err(status) => status,
            };
            cx.complete_production(status);
        }));
        let abort = task.abort_handle();
        {
            let mut slot = self.0.production.lock();
            let state = slot.as_mut().unwrap();
            if state.result.is_none() && self.status().is_none() {
                state.handler = Some(abort.clone());
            } else {
                abort.abort();
            }
        }
        let _ = start.send(());
        Ok(())
    }

    pub(super) fn own_response_handler(&self) -> Result<HandlerOwner, Status> {
        if self.role() != volo::context::Role::Server {
            return Err(Status::failed_precondition(
                "response producer requires a server call",
            ));
        }
        let backward = self.with_metainfo(super::metadata::ResponseMetainfo::new);
        {
            let mut slot = self.0.production.lock();
            if slot.is_some() || self.status().is_some() {
                return Err(Status::failed_precondition(
                    "response producer already started or RPC finished",
                ));
            }
            *slot = Some(Production {
                running: true,
                backward,
                ..Default::default()
            });
        }
        Ok(HandlerOwner(self.clone()))
    }

    pub(super) fn complete_production(&self, status: Status) {
        {
            let mut slot = self.0.production.lock();
            let state = slot.as_mut().expect("response handler owns production");
            if state.result.is_some() {
                return;
            }
            self.with_metainfo(|info| state.backward.publish(info));
            state.result = Some(status);
            state.ready = true;
            state.handler.take();
        }
        self.abort_children();
        self.0.production_changed.notify_waiters();
    }

    pub(super) fn abort_response_handler(&self) {
        let abort = self
            .0
            .production
            .lock()
            .as_mut()
            .and_then(|p| p.handler.take());
        if let Some(abort) = abort {
            abort.abort();
        }
    }
}

pub(super) struct HandlerOwner(StreamContext);
impl Drop for HandlerOwner {
    fn drop(&mut self) {
        self.0
            .complete_production(Status::cancelled("response handler dropped"));
        self.0.0.production.lock().as_mut().unwrap().running = false;
        self.0.0.children_changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, sync::Arc};

    use volo::{
        context::Role,
        streaming::{IoPhase, RpcError},
    };

    use super::*;

    #[tokio::test]
    async fn response_handler_inherits_latest_metainfo() {
        use metainfo::{Forward, METAINFO};
        let cx = StreamContext::new("/Echo/Chat", None, Role::Server, None);
        cx.update_metainfo(|info| info.set_persistent("layer", "latest"));
        let (sent, received) = tokio::sync::oneshot::channel();
        cx.start_response_handler(async move {
            let value = METAINFO.with(|info| info.borrow().get_persistent("layer"));
            sent.send(value).unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(received.await.unwrap().as_deref(), Some("latest"));
        cx.children_reclaimed().await;
    }

    #[tokio::test]
    async fn response_handler_publishes_backward_metainfo_before_headers_and_trailers() {
        use metainfo::{Backward, METAINFO};
        let cx = StreamContext::new("/Echo/Chat", None, Role::Server, None);
        let producer = cx.clone();
        let (resume, resumed) = tokio::sync::oneshot::channel();
        cx.start_response_handler(async move {
            METAINFO.with(|info| info.borrow_mut().set_backward_transient("phase", "header"));
            producer.commit_produced(|| ())?;
            resumed.await.unwrap();
            producer.update_metainfo(|info| info.set_backward_transient("phase", "trailer"));
            Ok(())
        })
        .unwrap();
        cx.response_ready().await.unwrap();
        let mut headers = http::HeaderMap::new();
        super::super::metadata::write_backward(&cx, &mut headers).unwrap();
        assert_eq!(
            headers
                .get("rpc-backward-phase")
                .map(|value| value.to_str().unwrap()),
            Some("header")
        );
        resume.send(()).unwrap();
        cx.children_reclaimed().await;
        let mut trailers = http::HeaderMap::new();
        super::super::metadata::write_backward(&cx, &mut trailers).unwrap();
        assert_eq!(
            trailers
                .get("rpc-backward-phase")
                .map(|value| value.to_str().unwrap()),
            Some("trailer")
        );
        assert!(
            cx.with_metainfo(|info| info.get_backward_transient("phase"))
                .is_none(),
            "task snapshots remain isolated"
        );
    }

    #[tokio::test]
    async fn response_metainfo_preserves_deletions_and_outer_layer_updates() {
        use metainfo::Backward;
        let cx = StreamContext::new("/Echo/Chat", None, Role::Server, None);
        cx.update_metainfo(|info| {
            info.set_backward_transient("removed", "initial");
            info.set_backward_transient("outer", "initial");
            info.set_backward_transient("changed", "initial");
        });
        let producer = cx.clone();
        let (resume, resumed) = tokio::sync::oneshot::channel();
        cx.start_response_handler(async move {
            producer.update_metainfo(|info| {
                info.del_backward_transient("removed");
                info.set_backward_transient("outer", "producer");
                info.set_backward_transient("changed", "header");
            });
            producer.commit_produced(|| ())?;
            resumed.await.unwrap();
            producer.update_metainfo(|info| {
                info.del_backward_transient("changed");
            });
            Ok(())
        })
        .unwrap();
        cx.response_ready().await.unwrap();
        cx.update_metainfo(|info| info.set_backward_transient("outer", "layer-after-call"));
        let mut headers = http::HeaderMap::new();
        super::super::metadata::write_backward(&cx, &mut headers).unwrap();
        assert!(!headers.contains_key("rpc-backward-removed"));
        assert_eq!(headers["rpc-backward-outer"], "layer-after-call");
        assert_eq!(headers["rpc-backward-changed"], "header");
        resume.send(()).unwrap();
        cx.children_reclaimed().await;
        let mut trailers = http::HeaderMap::new();
        super::super::metadata::write_backward(&cx, &mut trailers).unwrap();
        assert!(!trailers.contains_key("rpc-backward-removed"));
        assert!(!trailers.contains_key("rpc-backward-changed"));
        assert_eq!(trailers["rpc-backward-outer"], "layer-after-call");
    }

    #[test]
    fn commit_retains_local_source_without_overriding_body_or_write_failures() {
        for change in 0..6 {
            let cx = StreamContext::new("/Echo/Call", None, Role::Server, None);
            cx.attach_h2_owner();
            let _owner = cx.own_response_handler().unwrap();
            let original = Status::from(RpcError::Transport {
                source: Arc::new(std::io::Error::other("encoder failed")),
                phase: IoPhase::Encode,
                committed: false,
            });
            cx.complete_production(original.clone());
            let mut wire = match change {
                1 => Status::internal(original.message()),
                2 => Status::unavailable("body failure"),
                3 => Status::with_details(
                    original.code(),
                    original.message(),
                    bytes::Bytes::from_static(b"changed"),
                ),
                _ => original.clone(),
            }
            .to_header_map()
            .unwrap();
            if change == 4 {
                wire.insert("body-metadata", "kept".parse().unwrap());
            }
            let result = cx.commit_response(wire, |_| {
                if change == 5 {
                    Err(Status::from(RpcError::Transport {
                        source: Arc::new(std::io::Error::other("socket failed")),
                        phase: IoPhase::Write,
                        committed: true,
                    }))
                } else {
                    Ok(())
                }
            });
            assert_eq!(result.is_err(), change == 5);
            let status = &cx.status().unwrap().status;
            let source = status.source().and_then(|s| s.downcast_ref::<RpcError>());
            match change {
                0 | 4 => assert!(matches!(
                    source,
                    Some(RpcError::Transport {
                        phase: IoPhase::Encode,
                        committed: false,
                        ..
                    })
                )),
                5 => assert!(matches!(
                    source,
                    Some(RpcError::Transport {
                        phase: IoPhase::Write,
                        committed: true,
                        ..
                    })
                )),
                _ => assert!(source.is_none(), "body replaced the error: {status:?}"),
            }
            if change == 4 {
                assert_eq!(status.metadata().get("body-metadata").unwrap(), "kept");
            }
        }
    }
}
