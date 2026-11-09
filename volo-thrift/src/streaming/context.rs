use std::{
    cell::RefCell,
    future::Future,
    sync::Arc,
    task::{Context, Poll},
    thread::ThreadId,
};

use parking_lot::{Condvar, Mutex};
use pin_project::pin_project;
use tokio::sync::Notify;
use volo::context::{Endpoint, Extensions, Role, RpcInfo};

use super::{Metadata, StreamError, StreamStatus};
use crate::context::Config;

tokio::task_local! { pub static STREAM_CONTEXT: StreamContext; }

/// Authoritative, shared state for the entire stream, including queued messages.
#[derive(Clone, Debug)]
pub struct StreamContext {
    pub(crate) inner: Arc<Inner>,
}

/// Weak observer for periodic metrics; keeping it never retains a stream or buffer.
#[derive(Clone, Debug)]
pub struct StreamResourceMonitor {
    inner: std::sync::Weak<Inner>,
}
impl StreamResourceMonitor {
    pub fn snapshot(&self) -> Option<super::StreamResources> {
        self.inner.upgrade().map(|inner| inner.resources.snapshot())
    }
    pub fn is_finished(&self) -> bool {
        self.inner
            .upgrade()
            .is_none_or(|inner| inner.status.lock().is_some())
    }
}

#[derive(Debug)]
pub(crate) struct Inner {
    pub resources: super::resources::ResourceCounters,
    pub started_at: std::time::Instant,
    pub hooks: std::sync::OnceLock<super::HookChain>,
    pub deadline: Mutex<Option<std::time::Instant>>,
    pub metainfo: SharedMetainfo,
    pub children: Mutex<Vec<std::sync::Weak<Inner>>>,
    pub mode: std::sync::atomic::AtomicU8,
    pub id: i32,
    pub service: String,
    pub method: String,
    pub rpc_info: Mutex<RpcInfo<Config>>,
    pub extensions: Mutex<Extensions>,
    pub metadata: Mutex<Metadata>,
    pub status: Mutex<Option<StreamStatus>>,
    pub canceled: Mutex<Option<StreamError>>,
    pub notify: Notify,
    pub driver_notify: Mutex<Option<Arc<Notify>>>,
}

impl StreamContext {
    pub fn new(id: i32, service: impl Into<String>, method: impl Into<String>, role: Role) -> Self {
        let service = service.into();
        let method = method.into();
        let rpc_info = RpcInfo::new(
            role,
            method.clone().into(),
            Endpoint::default(),
            Endpoint::new(service.clone().into()),
            Config::default(),
        );
        Self {
            inner: Arc::new(Inner {
                resources: Default::default(),
                started_at: std::time::Instant::now(),
                hooks: Default::default(),
                deadline: Mutex::new(None),
                id,
                mode: std::sync::atomic::AtomicU8::new(0),
                children: Mutex::default(),
                metainfo: SharedMetainfo::default(),
                service,
                method,
                rpc_info: Mutex::new(rpc_info),
                extensions: Mutex::default(),
                metadata: Mutex::default(),
                status: Mutex::new(None),
                canceled: Mutex::new(None),
                notify: Notify::new(),
                driver_notify: Mutex::new(None),
            }),
        }
    }
    pub fn with_metainfo<T>(&self, f: impl FnOnce(&metainfo::MetaInfo) -> T) -> T {
        if self.is_current_owner() {
            return metainfo::METAINFO.with(|info| f(&info.borrow()));
        }
        let mut state = self.inner.metainfo.state.lock();
        while state.value.is_none() {
            self.inner.metainfo.ready.wait(&mut state);
        }
        f(state.value.as_ref().expect("available metainfo"))
    }
    pub fn update_metainfo<T>(&self, f: impl FnOnce(&mut metainfo::MetaInfo) -> T) -> T {
        if self.is_current_owner() {
            return metainfo::METAINFO.with(|info| f(&mut info.borrow_mut()));
        }
        let mut state = self.inner.metainfo.state.lock();
        while state.value.is_none() {
            self.inner.metainfo.ready.wait(&mut state);
        }
        f(state.value.as_mut().expect("available metainfo"))
    }
    fn is_current_owner(&self) -> bool {
        STREAM_CONTEXT
            .try_with(|cx| Arc::ptr_eq(&cx.inner, &self.inner))
            .unwrap_or(false)
            && self.inner.metainfo.state.lock().owner == Some(std::thread::current().id())
    }
    fn take_metainfo(&self) -> metainfo::MetaInfo {
        let mut state = self.inner.metainfo.state.lock();
        while state.value.is_none() {
            self.inner.metainfo.ready.wait(&mut state);
        }
        state.owner = Some(std::thread::current().id());
        state.value.take().expect("available metainfo")
    }
    fn put_metainfo(&self, value: metainfo::MetaInfo) {
        let mut state = self.inner.metainfo.state.lock();
        state.value = Some(value);
        state.owner = None;
        self.inner.metainfo.ready.notify_all();
    }
    pub fn mode(&self) -> super::StreamMode {
        use super::StreamMode;
        match self.inner.mode.load(std::sync::atomic::Ordering::Acquire) {
            1 => StreamMode::Client,
            2 => StreamMode::Server,
            3 => StreamMode::Bidirectional,
            _ => StreamMode::Raw,
        }
    }
    pub fn set_mode(&mut self, mode: super::StreamMode) {
        self.inner
            .mode
            .store(mode as u8, std::sync::atomic::Ordering::Release);
    }
    /// A local clock only. TTStream never serializes this value into HEADER.
    pub fn deadline(&self) -> Option<std::time::Instant> {
        *self.inner.deadline.lock()
    }
    pub fn set_deadline(&mut self, deadline: Option<std::time::Instant>) {
        *self.inner.deadline.lock() = deadline;
        self.wake_driver();
    }
    pub fn resource_monitor(&self) -> StreamResourceMonitor {
        StreamResourceMonitor {
            inner: Arc::downgrade(&self.inner),
        }
    }
    pub fn started_at(&self) -> std::time::Instant {
        self.inner.started_at
    }
    pub fn resources(&self) -> super::StreamResources {
        self.inner.resources.snapshot()
    }
    pub fn stream_id(&self) -> i32 {
        self.inner.id
    }
    pub fn service(&self) -> &str {
        &self.inner.service
    }
    pub fn method(&self) -> &str {
        &self.inner.method
    }
    pub fn metadata(&self) -> Metadata {
        self.inner.metadata.lock().clone()
    }
    pub fn update_metadata<T>(&self, f: impl FnOnce(&mut Metadata) -> T) -> T {
        f(&mut self.inner.metadata.lock())
    }
    pub fn with_rpc_info<T>(&self, f: impl FnOnce(&RpcInfo<Config>) -> T) -> T {
        f(&self.inner.rpc_info.lock())
    }
    pub fn update_rpc_info<T>(&self, f: impl FnOnce(&mut RpcInfo<Config>) -> T) -> T {
        f(&mut self.inner.rpc_info.lock())
    }
    pub fn with_extensions<T>(&self, f: impl FnOnce(&Extensions) -> T) -> T {
        f(&self.inner.extensions.lock())
    }
    pub fn update_extensions<T>(&self, f: impl FnOnce(&mut Extensions) -> T) -> T {
        f(&mut self.inner.extensions.lock())
    }
    pub fn status(&self) -> Option<StreamStatus> {
        self.inner.status.lock().clone()
    }
    pub fn cancellation(&self) -> Option<StreamError> {
        self.inner.canceled.lock().clone()
    }
    pub fn attach_child(&self, child: &StreamContext) {
        {
            let mut children = self.inner.children.lock();
            children.retain(|child| child.strong_count() > 0);
            children.push(Arc::downgrade(&child.inner));
        }
        // Register before reading terminal state: finish/cancel publishes state
        // before taking its children snapshot, so either side observes the child.
        if let Some(error) = self.cancellation().or_else(|| {
            self.status()
                .map(|status| Self::child_terminal_error(status.error))
        }) {
            self.cancel_child(child, &error);
        }
    }
    fn child_terminal_error(error: Option<StreamError>) -> StreamError {
        match error {
            Some(error @ StreamError::Canceled { .. }) => error,
            _ => StreamError::Canceled {
                cause: super::CancelCause::Local,
                code: 12012,
                message: "parent stream finished".into(),
                path: Vec::new(),
            },
        }
    }
    pub fn cancel_local(&self, message: impl Into<String>) {
        let caller = self.with_rpc_info(|rpc| rpc.caller().service_name().to_string());
        self.cancel(StreamError::Canceled {
            cause: super::CancelCause::Local,
            code: 12007,
            message: message.into(),
            path: if caller.is_empty() {
                Vec::new()
            } else {
                vec![caller]
            },
        });
    }
    pub fn cancel(&self, mut error: StreamError) {
        if let StreamError::Canceled { cause, path, .. } = &mut error {
            if path.is_empty()
                && matches!(
                    cause,
                    super::CancelCause::Local
                        | super::CancelCause::Deadline
                        | super::CancelCause::Drop
                )
                && self.with_rpc_info(|rpc| rpc.role() == Role::Client)
            {
                let caller = self.with_rpc_info(|rpc| rpc.caller().service_name().to_string());
                if !caller.is_empty() {
                    path.push(caller);
                }
            }
        }
        {
            let mut value = self.inner.canceled.lock();
            if value.is_some() {
                return;
            }
            *value = Some(error.clone());
        }
        self.inner.notify.notify_waiters();
        self.wake_driver();
        self.cancel_children(error);
    }
    fn wake_driver(&self) {
        if let Some(notify) = &*self.inner.driver_notify.lock() {
            notify.notify_one();
        }
    }
    fn cancel_children(&self, error: StreamError) {
        let children = self
            .inner
            .children
            .lock()
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .collect::<Vec<_>>();
        for inner in children {
            let child = StreamContext { inner };
            self.cancel_child(&child, &error);
        }
    }
    fn cancel_child(&self, child: &StreamContext, error: &StreamError) {
        let caller = child.with_rpc_info(|rpc| rpc.caller().service_name().to_string());
        let error = match error {
            StreamError::Canceled {
                cause,
                code,
                message,
                path,
            } => {
                let mut path = path.clone();
                path.push(caller);
                StreamError::Canceled {
                    cause: *cause,
                    code: *code,
                    message: message.clone(),
                    path,
                }
            }
            error => StreamError::Canceled {
                cause: super::CancelCause::Upstream,
                code: 12011,
                message: error.to_string(),
                path: vec!["non-ttstream path".into(), caller],
            },
        };
        child.cancel(error);
    }
    pub async fn cancelled(&self) -> StreamError {
        loop {
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(error) = self.cancellation() {
                return error;
            }
            notified.await;
        }
    }
    /// Runs a synchronous extension against the authoritative task-local view.
    pub fn sync_scope<R>(&self, operation: impl FnOnce() -> R) -> R {
        let future = HandlerScope {
            cx: self.clone(),
            future: async move { operation() },
        };
        futures::pin_mut!(future);
        let mut task = Context::from_waker(futures::task::noop_waker_ref());
        match future.poll(&mut task) {
            Poll::Ready(value) => value,
            Poll::Pending => unreachable!("synchronous operation"),
        }
    }
    /// Polls the future with this context and its metainfo in task-local scope.
    /// This does not cancel or abort the future. Spawned tasks that need to stop
    /// on cancellation must also poll [`Self::cancelled`] or be explicitly aborted.
    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        HandlerScope {
            cx: self.clone(),
            future,
        }
        .await
    }
    pub(crate) fn finish(&self, status: StreamStatus) -> bool {
        let mut slot = self.inner.status.lock();
        if slot.is_some() {
            return false;
        }
        let terminal_error = status.error.clone();
        *slot = Some(status);
        drop(slot);
        self.inner.notify.notify_waiters();
        self.wake_driver();
        self.cancel_children(Self::child_terminal_error(terminal_error));
        true
    }
}

pub(crate) struct SharedMetainfo {
    state: Mutex<MetaState>,
    ready: Condvar,
}
struct MetaState {
    value: Option<metainfo::MetaInfo>,
    owner: Option<ThreadId>,
}
impl Default for SharedMetainfo {
    fn default() -> Self {
        Self {
            state: Mutex::new(MetaState {
                value: Some(metainfo::MetaInfo::default()),
                owner: None,
            }),
            ready: Condvar::new(),
        }
    }
}
impl std::fmt::Debug for SharedMetainfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedMetainfo { .. }")
    }
}

// A scope lends the single MetaInfo value to the existing task-local only during
// poll. No lock is held while polling user code or while a future is Pending.
// Access through the explicit context reuses this owner when called reentrantly.
#[pin_project]
struct HandlerScope<F> {
    cx: StreamContext,
    #[pin]
    future: F,
}
struct ReturnOwner(StreamContext);
impl Drop for ReturnOwner {
    fn drop(&mut self) {
        metainfo::METAINFO
            .with(|slot| self.0.put_metainfo(std::mem::take(&mut *slot.borrow_mut())));
    }
}
struct ResumeParent(Option<StreamContext>);
impl Drop for ResumeParent {
    fn drop(&mut self) {
        if let Some(parent) = &self.0 {
            let value = parent.take_metainfo();
            metainfo::METAINFO.with(|slot| *slot.borrow_mut() = value);
        }
    }
}
impl<F: Future> Future for HandlerScope<F> {
    type Output = F::Output;
    fn poll(self: std::pin::Pin<&mut Self>, task: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        if this.cx.is_current_owner() {
            return this.future.poll(task);
        }
        // Suspending the outer owner also makes explicit accesses to that
        // context from a nested scope safe; the outer task-local stays empty
        // until the inner scope restores it through ResumeParent.
        let parent = STREAM_CONTEXT
            .try_with(Clone::clone)
            .ok()
            .filter(|cx| cx.is_current_owner());
        if let Some(parent) = &parent {
            metainfo::METAINFO
                .with(|slot| parent.put_metainfo(std::mem::take(&mut *slot.borrow_mut())));
        }
        let _resume = ResumeParent(parent);
        let value = this.cx.take_metainfo();
        STREAM_CONTEXT.sync_scope(this.cx.clone(), || {
            metainfo::METAINFO.sync_scope(RefCell::new(value), || {
                let _return_owner = ReturnOwner(this.cx.clone());
                this.future.poll(task)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use metainfo::{Forward, METAINFO};

    use super::*;
    fn context(name: &str) -> StreamContext {
        StreamContext::new(1, name, "test", Role::Server)
    }
    #[tokio::test]
    async fn tasklocal_and_explicit_context_share_one_owner_across_pending() {
        let cx = context("outer");
        cx.scope(async {
            METAINFO.with(|mi| mi.borrow_mut().set_transient("a", "first"));
            assert_eq!(
                cx.with_metainfo(|mi| mi.get_transient("a")).as_deref(),
                Some("first")
            );
            tokio::task::yield_now().await;
            cx.update_metainfo(|mi| mi.set_transient("a", "second"));
            assert_eq!(
                METAINFO
                    .with(|mi| mi.borrow().get_transient("a"))
                    .as_deref(),
                Some("second")
            );
        })
        .await;
        assert_eq!(
            cx.with_metainfo(|mi| mi.get_transient("a")).as_deref(),
            Some("second")
        );
    }
    #[tokio::test]
    async fn nested_context_can_visit_outer_owner_without_forking() {
        let outer = context("outer");
        let inner = context("inner");
        outer
            .scope(async {
                outer.update_metainfo(|mi| mi.set_persistent("a", "outer"));
                inner
                    .scope(async {
                        outer.update_metainfo(|mi| mi.set_persistent("a", "updated"));
                        inner.update_metainfo(|mi| mi.set_persistent("a", "inner"));
                        tokio::task::yield_now().await;
                        assert_eq!(
                            outer.with_metainfo(|mi| mi.get_persistent("a")).as_deref(),
                            Some("updated")
                        );
                    })
                    .await;
                assert_eq!(
                    METAINFO
                        .with(|mi| mi.borrow().get_persistent("a"))
                        .as_deref(),
                    Some("updated")
                );
            })
            .await;
    }
    #[tokio::test]
    async fn panic_and_future_drop_return_the_owner() {
        use futures::FutureExt;
        let cx = context("panic");
        let result = std::panic::AssertUnwindSafe(cx.scope(async {
            cx.update_metainfo(|mi| mi.set_transient("a", "kept"));
            panic!("test panic");
        }))
        .catch_unwind()
        .await;
        assert!(result.is_err());
        assert_eq!(
            cx.with_metainfo(|mi| mi.get_transient("a")).as_deref(),
            Some("kept")
        );
        let mut scope = Box::pin(cx.scope(std::future::pending::<()>()));
        assert!(futures::poll!(&mut scope).is_pending());
        drop(scope);
        cx.update_metainfo(|mi| mi.set_transient("a", "released"));
    }
    #[test]
    fn sync_scope_updates_the_same_owner_and_restores_nested_contexts() {
        let outer = context("outer");
        let inner = context("inner");
        outer.sync_scope(|| {
            METAINFO.with(|info| info.borrow_mut().set_persistent("trace", "parent"));
            inner.sync_scope(|| {
                outer.update_metainfo(|info| info.set_persistent("trace", "updated"));
                METAINFO.with(|info| info.borrow_mut().set_transient("trace", "child"));
            });
            assert_eq!(
                METAINFO
                    .with(|info| info.borrow().get_persistent("trace"))
                    .as_deref(),
                Some("updated")
            );
        });
        assert_eq!(
            inner
                .with_metainfo(|info| info.get_transient("trace"))
                .as_deref(),
            Some("child")
        );
    }
    #[test]
    fn resource_monitor_does_not_retain_the_context() {
        let cx = context("observe");
        let monitor = cx.resource_monitor();
        assert!(monitor.snapshot().is_some());
        assert!(!monitor.is_finished());
        drop(cx);
        assert!(monitor.snapshot().is_none());
        assert!(monitor.is_finished());
    }
    #[test]
    fn cascading_cancel_uses_child_caller_name_and_preserves_code() {
        let parent = context("IDLService");
        let child = StreamContext::new(2, "IDLService", "call", Role::Client);
        child.update_rpc_info(|rpc| rpc.caller_mut().set_service_name("B".into()));
        parent.attach_child(&child);
        parent.cancel(StreamError::Canceled {
            cause: super::super::CancelCause::Upstream,
            code: 12015,
            message: "timeout".into(),
            path: vec!["A".into()],
        });
        assert!(
            matches!(child.cancellation(),Some(StreamError::Canceled {code:12015,path,..}) if path==["A","B"])
        );
        let late = StreamContext::new(3, "IDLService", "late", Role::Client);
        late.update_rpc_info(|rpc| rpc.caller_mut().set_service_name("B".into()));
        parent.attach_child(&late);
        assert!(
            matches!(late.cancellation(),Some(StreamError::Canceled {code:12015,path,..}) if path==["A","B"])
        );
        let completed = context("finished");
        let detached = context("child");
        detached.update_rpc_info(|rpc| rpc.caller_mut().set_service_name("B".into()));
        completed.attach_child(&detached);
        completed.finish(StreamStatus::default());
        assert!(
            matches!(detached.cancellation(),Some(StreamError::Canceled {code:12012,path,..}) if path==["B"])
        );
    }
    #[test]
    fn child_attached_after_successful_parent_finish_is_canceled() {
        let parent = context("parent");
        parent.finish(StreamStatus::default());
        let child = context("child");
        child.update_rpc_info(|rpc| rpc.caller_mut().set_service_name("B".into()));
        parent.attach_child(&child);
        assert!(
            matches!(child.cancellation(), Some(StreamError::Canceled { code: 12012, path, .. }) if path == ["B"])
        );
        assert!(parent.cancellation().is_none());
    }
}
