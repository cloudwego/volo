//! Shared handler scope for the existing ping-pong business future.

use std::{future::Future, panic::AssertUnwindSafe};

use futures::FutureExt;
use volo::{
    context::Context,
    streaming::{RpcResult, StreamContext as SharedContext},
};

use super::{StreamContext, StreamStatus};
use crate::{ServerError, context::ServerContext};

/// Native ping-pong/multiplex owner. The driver keeps accepted handlers alive on peer EOF.
#[derive(Default)]
pub(crate) struct ConnectionOwner {
    calls: parking_lot::Mutex<rustc_hash::FxHashMap<usize, StreamContext>>,
    reclaim: parking_lot::Mutex<Vec<StreamContext>>,
    closed: std::sync::atomic::AtomicBool,
    closed_notify: tokio::sync::Notify,
    #[cfg(feature = "multiplex")]
    multiplex: std::sync::OnceLock<tokio::sync::Mutex<MultiplexTasks>>,
}
/// The native task handles are transferred here only when their connection is
/// forcibly dropped. Normal requests continue to use the driver's own JoinSet.
#[cfg(feature = "multiplex")]
#[derive(Default)]
pub(crate) struct MultiplexTasks {
    pub(crate) handlers: tokio::task::JoinSet<()>,
    pub(crate) writer: Option<tokio::task::JoinHandle<()>>,
}
tokio::task_local! { static CONNECTION: std::sync::Arc<ConnectionOwner>; }
impl ConnectionOwner {
    pub(crate) fn current() -> Option<std::sync::Arc<Self>> {
        CONNECTION.try_with(Clone::clone).ok()
    }
    pub(crate) async fn scope<F: Future>(self: std::sync::Arc<Self>, future: F) -> F::Output {
        CONNECTION.scope(self, future).await
    }
    #[cfg(feature = "multiplex")]
    pub(crate) fn reclaim_multiplex(&self, tasks: MultiplexTasks) {
        assert!(
            self.multiplex.set(tokio::sync::Mutex::new(tasks)).is_ok(),
            "a native connection owns one multiplex driver"
        );
    }
}
impl volo::streaming::NativeConnection for ConnectionOwner {
    fn cancel(&self, error: volo::streaming::RpcError) {
        let contexts: Vec<_> = self.calls.lock().values().cloned().collect();
        for cx in &contexts {
            cx.cancel(error.clone().into());
        }
        self.reclaim.lock().extend(contexts);
    }
    fn active_calls(&self) -> usize {
        self.calls
            .lock()
            .values()
            .filter(|cx| cx.status().is_none())
            .count()
    }
    fn active_tasks(&self) -> usize {
        let calls = self.calls.lock();
        let contexts = calls
            .values()
            .chain(self.reclaim.lock().iter())
            .cloned()
            .collect::<Vec<_>>();
        let mut ids = std::collections::HashSet::new();
        calls.len()
            + contexts
                .iter()
                .filter(|cx| ids.insert(std::sync::Arc::as_ptr(&cx.inner) as usize))
                .map(StreamContext::active_tasks)
                .sum::<usize>()
    }
    fn reclaimed(&self) -> futures::future::BoxFuture<'_, ()> {
        Box::pin(async {
            // Run-owner drop can start reclamation before the connection future
            // is dropped. Wait until its native task guards have transferred
            // their handles, rather than taking an empty snapshot too early.
            loop {
                let closed = self.closed_notify.notified();
                if self.closed.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                closed.await;
            }
            #[cfg(feature = "multiplex")]
            if let Some(tasks) = self.multiplex.get() {
                let mut tasks = tasks.lock().await;
                while tasks.handlers.join_next().await.is_some() {}
                if let Some(writer) = tasks.writer.as_mut() {
                    let _ = writer.await;
                }
                tasks.writer = None;
            }
            // Keep ownership in the connection across await: if this waiter is
            // itself canceled, the next waiter must still join all cleanup.
            let contexts = self.reclaim.lock().clone();
            for cx in contexts {
                cx.tasks_reclaimed().await;
            }
            self.reclaim.lock().clear();
        })
    }
}
pub(crate) struct ConnectionGuard(pub std::sync::Arc<ConnectionOwner>);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        volo::streaming::NativeConnection::cancel(
            &*self.0,
            volo::streaming::RpcError::ServerDraining,
        );
        self.0
            .closed
            .store(true, std::sync::atomic::Ordering::Release);
        self.0.closed_notify.notify_waiters();
    }
}

/// The connection driver continues to own this future. No read/EOF watcher is
/// installed: accepted ping-pong business survives a peer disconnect.
pub async fn dispatch_unary<T, H, F>(
    context: &mut ServerContext,
    service: &str,
    handler: H,
) -> Result<T, ServerError>
where
    T: Send,
    H: FnOnce(SharedContext) -> F + Send,
    F: Future<Output = RpcResult<T>> + Send,
{
    let snapshot = metainfo::METAINFO.try_with(|parent| std::mem::take(&mut *parent.borrow_mut()));
    let has_parent = snapshot.is_ok();
    let native = StreamContext::with_metainfo_snapshot(
        context.seq_id.unwrap_or_default(),
        service,
        context.rpc_info().method().as_str(),
        volo::context::Role::Server,
        snapshot.unwrap_or_default(),
    );
    std::mem::swap(&mut *native.inner.rpc_info.lock(), context.rpc_info_mut());
    std::mem::swap(
        &mut *native.inner.extensions.lock(),
        &mut context.extensions,
    );
    native
        .inner
        .mode
        .store(4, std::sync::atomic::Ordering::Release);
    native
        .inner
        .registered_method
        .store(true, std::sync::atomic::Ordering::Release);
    let connection = ConnectionOwner::current();
    if let Some(connection) = &connection {
        connection.calls.lock().insert(
            std::sync::Arc::as_ptr(&native.inner) as usize,
            native.clone(),
        );
    }
    let owner = UnaryOwner {
        native,
        has_parent,
        context,
        connection,
    };
    let shared = owner.native.shared_context();
    if let Some(scope) = shared.with_extensions(|extensions| {
        extensions
            .get::<std::sync::Arc<dyn volo::streaming::StreamScope>>()
            .cloned()
    }) {
        shared.set_scope(scope);
    }
    let cancellation = owner.native.clone();
    let result = AssertUnwindSafe(shared.scope(async {
        shared.open_hooks()?;
        tokio::select! { biased;
            error=cancellation.cancelled()=>Err(error.into()),
            result=shared.invoke(handler)=>result,
        }
    }))
    .catch_unwind()
    .await
    .unwrap_or_else(|_| {
        Err(volo::streaming::RpcError::Application {
            kind: pilota::thrift::ApplicationExceptionKind::INTERNAL_ERROR.as_i32(),
            message: "unary handler panicked".into(),
        })
    });
    // A handler may cancel and return Ready in the same poll. Only explicit
    // local cancellation reaches this state; peer EOF has no watcher here.
    let result = match owner.native.cancellation() {
        Some(error) => Err(error.into()),
        None => result,
    };
    owner.native.finish(StreamStatus {
        error: result.as_ref().err().cloned().map(Into::into),
        ..Default::default()
    });
    owner.native.tasks_reclaimed().await;
    result.map_err(Into::into)
}

struct UnaryOwner<'a> {
    native: StreamContext,
    has_parent: bool,
    context: &'a mut ServerContext,
    connection: Option<std::sync::Arc<ConnectionOwner>>,
}
impl Drop for UnaryOwner<'_> {
    fn drop(&mut self) {
        if self.native.inner.status.lock().is_none() {
            self.native.finish(StreamStatus {
                error: Some(self.native.cancellation().unwrap_or_else(|| {
                    if std::thread::panicking() {
                        super::StreamError::Application(pilota::thrift::ApplicationException::new(
                            pilota::thrift::ApplicationExceptionKind::INTERNAL_ERROR,
                            "unary handler panicked",
                        ))
                    } else {
                        super::StreamError::Canceled {
                            cause: super::CancelCause::Drop,
                            code: 0,
                            message: "unary handler future dropped".into(),
                            path: Default::default(),
                        }
                    }
                })),
                ..Default::default()
            });
        }
        if let Some(connection) = &self.connection {
            connection
                .calls
                .lock()
                .remove(&(std::sync::Arc::as_ptr(&self.native.inner) as usize));
        }
        std::mem::swap(
            &mut *self.native.inner.rpc_info.lock(),
            self.context.rpc_info_mut(),
        );
        std::mem::swap(
            &mut *self.native.inner.extensions.lock(),
            &mut self.context.extensions,
        );
        if self.has_parent {
            let info = self.native.update_metainfo(std::mem::take);
            let _ = metainfo::METAINFO.try_with(|parent| *parent.borrow_mut() = info);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use metainfo::{Forward, METAINFO};

    use super::*;

    #[tokio::test]
    async fn notify_waiters_remembers_an_unpolled_reclamation_waiter() {
        let owner = std::sync::Arc::new(ConnectionOwner::default());
        let closed = owner.closed_notify.notified();
        assert!(!owner.closed.load(std::sync::atomic::Ordering::Acquire));
        drop(ConnectionGuard(owner.clone()));
        tokio::time::timeout(std::time::Duration::from_millis(100), closed)
            .await
            .unwrap();
        volo::streaming::NativeConnection::reclaimed(&*owner).await;
    }

    #[tokio::test]
    async fn an_explicit_local_cancel_ends_pending_work_and_wins_over_a_ready_success() {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            for ready in [true, false] {
                let mut context = ServerContext::default();
                let result = dispatch_unary(&mut context, "Service", move |cx| async move {
                    cx.cancel(volo::streaming::RpcError::Canceled {
                        cause: super::super::CancelCause::Local,
                        code: 0,
                        message: "stop".into(),
                        path: Default::default(),
                    });
                    if ready {
                        Ok(())
                    } else {
                        futures::future::pending().await
                    }
                })
                .await;
                assert!(
                    result.is_err(),
                    "explicit cancellation must fail the RPC: {result:?}"
                );
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn business_error_and_panic_restore_the_original_rpc_extensions_and_metainfo() {
        for panic in [false, true] {
            let mut context = ServerContext::default();
            context.rpc_info_mut().set_method("Get".into());
            context
                .rpc_info_mut()
                .caller_mut()
                .set_service_name("caller".into());
            context.extensions.insert(41_u64);
            METAINFO
                .scope(RefCell::new(Default::default()), async {
                    METAINFO.with(|slot| slot.borrow_mut().set_transient("request", "present"));
                    let result = dispatch_unary(&mut context, "Service", move |cx| async move {
                        assert_eq!(cx.mode(), Some(volo::streaming::RpcMode::Unary));
                        assert_eq!(SharedContext::current().unwrap().id(), cx.id());
                        cx.with_extensions(|extensions| {
                            assert_eq!(extensions.get::<u64>(), Some(&41));
                            extensions.insert(43_u64);
                        });
                        assert_eq!(
                            METAINFO.with(|slot| slot
                                .borrow()
                                .get_transient("request")
                                .unwrap()
                                .to_string()),
                            "present"
                        );
                        METAINFO
                            .with(|slot| slot.borrow_mut().set_transient("response", "preserved"));
                        assert!(!panic, "handler panic");
                        Err::<(), _>(volo::streaming::RpcError::Biz {
                            code: 17,
                            message: "failed".into(),
                            extra: Default::default(),
                        })
                    })
                    .await;
                    if panic {
                        assert!(matches!(result, Err(ServerError::Application(_))));
                    } else {
                        assert!(
                            matches!(result,Err(ServerError::Biz(error)) if error.status_code==17)
                        );
                    }
                    assert_eq!(context.rpc_info().method().as_str(), "Get");
                    assert_eq!(
                        context.rpc_info().caller().service_name().as_str(),
                        "caller"
                    );
                    assert_eq!(context.extensions.get::<u64>(), Some(&43));
                    assert_eq!(
                        METAINFO.with(|slot| slot
                            .borrow()
                            .get_transient("response")
                            .unwrap()
                            .to_string()),
                        "preserved"
                    );
                })
                .await;
        }
    }
}
