use std::{
    collections::BTreeMap,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use parking_lot::Mutex;
use tokio::sync::Notify;
use volo::context::{Endpoint, Extensions, MetaInfoScope as SharedMetainfo, Role, RpcInfo};

use super::{Metadata, StreamError, StreamStatus};
use crate::context::Config;

// Marks fully entered scopes. A hook may install a scope during admission,
// so identity alone cannot suppress a nested entry before that new scope runs.
tokio::task_local! { static ENTERED_SCOPE: (usize, u8); }

tokio::task_local! { pub(crate) static STREAM_CONTEXT: StreamContext; }

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
    tasks: Mutex<BTreeMap<usize, tokio::task::AbortHandle>>,
    task_sequence: AtomicUsize,
    tasks_changed: Notify,
    pub mode: std::sync::atomic::AtomicU8,
    pub registered_method: std::sync::atomic::AtomicBool,
    pub id: i32,
    pub service: pilota::FastStr,
    pub method: pilota::FastStr,
    pub rpc_info: Mutex<RpcInfo<Config>>,
    pub extensions: Mutex<Extensions>,
    pub middleware: Option<volo::streaming::ServerMiddleware>,
    pub common_hooks: std::sync::OnceLock<volo::streaming::CallHooks>,
    pub common_scope: std::sync::OnceLock<volo::streaming::SharedScope>,
    pub metadata: Mutex<Metadata>,
    pub status: Mutex<Option<StreamStatus>>,
    pub canceled: Mutex<Option<StreamError>>,
    pub notify: Notify,
    pub driver_notify: Mutex<Option<DriverNotify>>,
    pub(super) outgoing: Mutex<ResponseMetadata>,
}

pub(crate) type DirtyStreams = rustc_hash::FxHashMap<usize, std::sync::Weak<Inner>>;

#[derive(Debug)]
pub(crate) struct DriverNotify {
    pub wake: Arc<Notify>,
    pub changed: Arc<Mutex<DirtyStreams>>,
}
impl DriverNotify {
    pub fn notify_one(&self) {
        self.wake.notify_one();
    }
    fn changed(&self, cx: &StreamContext) {
        self.changed
            .lock()
            .insert(Arc::as_ptr(&cx.inner) as usize, Arc::downgrade(&cx.inner));
        self.wake.notify_one();
    }
}

#[derive(Debug, Default)]
pub(super) struct ResponseMetadata {
    pub(super) header: Metadata,
    pub(super) trailer: Metadata,
    pub(super) header_closed: bool,
    pub(crate) trailer_closed: bool,
    pub(crate) production_closed: bool,
}

impl Inner {
    pub(crate) fn producer_error(&self) -> Option<StreamError> {
        let cancellation = self.canceled.lock().clone();
        if cancellation.is_some() {
            return cancellation;
        }
        let completion = self.status.lock().as_ref().map(|status| {
            status
                .error
                .clone()
                .unwrap_or_else(|| StreamContext::child_terminal_error(None))
        });
        if completion.is_some() {
            return completion;
        }
        self.outgoing
            .lock()
            .production_closed
            .then(|| StreamContext::child_terminal_error(None))
    }
}

struct TaskOwner {
    cx: StreamContext,
    id: usize,
    exited: Arc<AtomicBool>,
}

impl Drop for TaskOwner {
    fn drop(&mut self) {
        self.exited.store(true, Ordering::Release);
        self.cx.inner.tasks.lock().remove(&self.id);
        self.cx.inner.tasks_changed.notify_waiters();
        if std::thread::panicking() {
            self.cx.cancel(StreamError::Application(
                pilota::thrift::ApplicationException::new(
                    pilota::thrift::ApplicationExceptionKind::INTERNAL_ERROR,
                    "managed stream task panicked",
                ),
            ));
        }
    }
}

impl StreamContext {
    pub fn current() -> Option<Self> {
        STREAM_CONTEXT.try_with(Clone::clone).ok()
    }

    pub fn new(id: i32, service: impl Into<String>, method: impl Into<String>, role: Role) -> Self {
        Self::with_metainfo_snapshot(id, service, method, role, Default::default())
    }

    pub(crate) fn with_metainfo_snapshot(
        id: i32,
        service: impl Into<String>,
        method: impl Into<String>,
        role: Role,
        info: metainfo::MetaInfo,
    ) -> Self {
        let service: pilota::FastStr = service.into().into();
        let method: pilota::FastStr = method.into().into();
        let rpc_info = RpcInfo::new(
            role,
            method.clone(),
            Endpoint::default(),
            Endpoint::new(service.clone()),
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
                registered_method: std::sync::atomic::AtomicBool::new(false),
                children: Mutex::default(),
                tasks: Mutex::default(),
                task_sequence: AtomicUsize::new(1),
                tasks_changed: Notify::new(),
                metainfo: SharedMetainfo::new(info),
                service,
                method,
                rpc_info: Mutex::new(rpc_info),
                extensions: Mutex::default(),
                middleware: (role == Role::Server)
                    .then(volo::streaming::ServerMiddleware::current)
                    .flatten(),
                common_hooks: Default::default(),
                common_scope: Default::default(),
                metadata: Mutex::default(),
                status: Mutex::new(None),
                canceled: Mutex::new(None),
                notify: Notify::new(),
                driver_notify: Mutex::new(None),
                outgoing: Mutex::default(),
            }),
        }
    }
    /// The public streaming view retains this call's existing native state.
    pub fn shared_context(&self) -> volo::streaming::StreamContext {
        volo::streaming::StreamContext::from_driver(self.inner.clone())
    }
    pub(crate) fn observe_header(
        &self,
        direction: volo::streaming::StreamDirection,
        metadata: &Metadata,
    ) {
        if let Some(hooks) = self.inner.common_hooks.get() {
            hooks.header(
                &self.shared_context(),
                direction,
                &super::shared::metadata_to_shared(metadata),
            );
        }
    }
    pub(crate) fn observe_meta(
        &self,
        direction: volo::streaming::StreamDirection,
        metadata: &Metadata,
    ) {
        if let Some(hooks) = self.inner.common_hooks.get() {
            hooks.meta(
                &self.shared_context(),
                direction,
                &super::shared::metadata_to_shared(metadata),
            );
        }
    }
    pub(crate) fn observe_trailer(
        &self,
        direction: volo::streaming::StreamDirection,
        metadata: &Metadata,
    ) {
        if let Some(hooks) = self.inner.common_hooks.get() {
            hooks.trailer(
                &self.shared_context(),
                direction,
                &super::shared::metadata_to_shared(metadata),
            );
        }
    }
    pub(crate) fn observe_half_close(&self, direction: volo::streaming::StreamDirection) {
        if let Some(hooks) = self.inner.common_hooks.get() {
            hooks.half_close(&self.shared_context(), direction);
        }
    }
    pub(crate) fn observe_sent_trailer(&self) {
        if let Some(hooks) = self.inner.common_hooks.get() {
            let metadata = self.inner.outgoing.lock().trailer.clone();
            let cx = self.shared_context();
            hooks.trailer(
                &cx,
                volo::streaming::StreamDirection::Send,
                &super::shared::metadata_to_shared(&metadata),
            );
            hooks.half_close(&cx, volo::streaming::StreamDirection::Send);
        }
    }
    pub(crate) fn observe_send(&self, size: usize) {
        if let Some(hooks) = self.inner.common_hooks.get() {
            hooks.send(&self.shared_context(), size);
        }
    }
    pub(crate) fn observe_recv(&self, size: usize) {
        if let Some(hooks) = self.inner.common_hooks.get() {
            hooks.recv(&self.shared_context(), size);
        }
    }
    pub(crate) fn observe_io_error(&self, error: &StreamError) {
        if let Some(hooks) = self.inner.common_hooks.get() {
            hooks.io_error(&self.shared_context(), &error.clone().into());
        }
    }

    pub(crate) fn initialize_outgoing(&self, client: bool) {
        self.inner.outgoing.lock().header_closed |= client;
    }

    pub(crate) fn set_outgoing_metadata(
        &self,
        metadata: Metadata,
        trailer: bool,
    ) -> Result<(), StreamError> {
        let mut outgoing = self.inner.outgoing.lock();
        let closed = if trailer {
            outgoing.trailer_closed
        } else {
            outgoing.header_closed
        };
        if closed
            || outgoing.production_closed
            || self.status().is_some()
            || self.cancellation().is_some()
        {
            return Err(StreamError::Protocol(
                "outgoing metadata already committed".into(),
            ));
        }
        if trailer {
            outgoing.trailer.extend(metadata);
        } else {
            outgoing.header.extend(metadata);
        }
        Ok(())
    }

    // Closing production leaves the native driver free to flush queued DATA and
    // the final Trailer. It neither cancels the wire stream nor commits status.
    pub(crate) fn close_production(&self) {
        self.inner.outgoing.lock().production_closed = true;
        self.inner.notify.notify_waiters();
        self.abort_tasks();
    }

    pub(crate) fn producer_error(&self) -> Option<StreamError> {
        self.inner.producer_error()
    }

    // The enqueue and the handler's close boundary share this short lock, so a
    // producer waiting for queue capacity can never enqueue after the Trailer.
    pub(crate) fn send_if_open<R>(&self, send: impl FnOnce() -> R) -> Result<R, StreamError> {
        let outgoing = self.inner.outgoing.lock();
        if outgoing.production_closed {
            return Err(Self::child_terminal_error(None));
        }
        Ok(send())
    }

    /// Starts one task owned by the same native cancellation and finish state.
    pub fn spawn_scoped<F>(
        &self,
        future: F,
    ) -> tokio::task::JoinHandle<Result<F::Output, StreamError>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let id = self.inner.task_sequence.fetch_add(1, Ordering::Relaxed);
        let exited = Arc::new(AtomicBool::new(false));
        let owner = TaskOwner {
            cx: self.clone(),
            id,
            exited: exited.clone(),
        };
        let cx = self.clone();
        let (start, started) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(self.inner.metainfo.task_scope(async move {
            let _owner = owner;
            if started.await.is_err() {
                return Err(StreamError::closed());
            }
            cx.scope(async {
                tokio::select! { biased;
                    error = cx.ended() => Err(error),
                    value = future => Ok(value),
                }
            })
            .await
        }));
        let abort = task.abort_handle();
        self.inner.tasks.lock().insert(id, abort.clone());
        if exited.load(Ordering::Acquire) {
            self.inner.tasks.lock().remove(&id);
            self.inner.tasks_changed.notify_waiters();
        }
        if self.producer_error().is_some() {
            abort.abort();
        }
        let _ = start.send(());
        task
    }

    pub(crate) fn active_tasks(&self) -> usize {
        self.inner.tasks.lock().len()
    }

    pub async fn tasks_reclaimed(&self) {
        loop {
            let notified = self.inner.tasks_changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.inner.tasks.lock().is_empty() {
                return;
            }
            notified.await;
        }
    }

    fn abort_tasks(&self) {
        let tasks = self
            .inner
            .tasks
            .lock()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for task in tasks {
            task.abort();
        }
    }

    pub(crate) async fn ended(&self) -> StreamError {
        loop {
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(error) = self.producer_error() {
                return error;
            }
            notified.await;
        }
    }
    pub fn with_metainfo<T>(&self, f: impl FnOnce(&metainfo::MetaInfo) -> T) -> T {
        self.inner.metainfo.with(f)
    }
    pub fn update_metainfo<T>(&self, f: impl FnOnce(&mut metainfo::MetaInfo) -> T) -> T {
        self.inner.metainfo.update(f)
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
    /// Borrow the existing service name without allocating a new string.
    pub fn service_faststr(&self) -> &pilota::FastStr {
        &self.inner.service
    }
    /// Borrow the existing method name without allocating a new string.
    pub fn method_faststr(&self) -> &pilota::FastStr {
        &self.inner.method
    }
    /// Whether this request matched a declared server method.
    pub fn is_registered_route(&self) -> bool {
        self.inner.registered_method.load(Ordering::Acquire)
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
        self.close_production();
        self.wake_driver();
        self.cancel_children(error);
    }
    pub(crate) fn wake_driver(&self) {
        if let Some(notify) = &*self.inner.driver_notify.lock() {
            notify.changed(self);
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
        if self.inner.is_entered() {
            return operation();
        }
        let state = self.inner.scope_state();
        let shared = self.shared_context();
        let operation = || ENTERED_SCOPE.sync_scope(state, operation);
        shared.sync_scope_current(|| {
            STREAM_CONTEXT.sync_scope(self.clone(), || {
                self.inner
                    .metainfo
                    .sync_scope(|| match self.inner.common_scope.get() {
                        Some(scope) => scope.enter(&shared, operation),
                        None => operation(),
                    })
            })
        })
    }
    /// Polls the future with this context and its metainfo in task-local scope.
    /// This does not cancel the future. Spawned tasks must also listen for cancellation.
    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        tokio::pin!(future);
        futures::future::poll_fn(|task| self.sync_scope(|| future.as_mut().poll(task))).await
    }
    pub(crate) fn finish(&self, status: StreamStatus) -> bool {
        let mut slot = self.inner.status.lock();
        if slot.is_some() {
            return false;
        }
        let terminal_error = status.error.clone();
        *slot = Some(status);
        drop(slot);
        self.close_production();
        self.wake_driver();
        self.cancel_children(Self::child_terminal_error(terminal_error));
        // Flow waits for its pending I/O observations before closing stream hooks.
        if self.inner.mode.load(Ordering::Acquire) == 4 {
            if let Some(hooks) = self.inner.common_hooks.get() {
                hooks.close(&self.shared_context());
            }
        }
        true
    }
}

impl Inner {
    pub(super) fn scope_state(&self) -> (usize, u8) {
        (
            self as *const Self as usize,
            u8::from(self.common_scope.get().is_some()),
        )
    }
    pub(super) fn is_entered(&self) -> bool {
        volo::streaming::StreamContext::is_current_driver(self)
            && self.metainfo.is_current()
            && ENTERED_SCOPE
                .try_with(|current| *current == self.scope_state())
                .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use metainfo::{Forward, METAINFO};

    use super::*;
    fn context(name: &str) -> StreamContext {
        StreamContext::new(1, name, "test", Role::Server)
    }
    #[test]
    fn nested_entry_reuses_scope_and_late_installation_is_visible() {
        struct Scope(Arc<AtomicUsize>);
        impl volo::streaming::StreamScope for Scope {
            fn enter(&self, _: &volo::streaming::StreamContext, operation: &mut dyn FnMut()) {
                self.0.fetch_add(1, Ordering::Relaxed);
                operation();
            }
        }
        let cx = context("scope");
        let entered = Arc::new(AtomicUsize::new(0));
        cx.sync_scope(|| {
            assert!(
                cx.shared_context()
                    .set_scope(Arc::new(Scope(entered.clone())))
            );
            cx.sync_scope(|| {
                assert_eq!(entered.load(Ordering::Relaxed), 1);
                cx.sync_scope(|| assert_eq!(entered.load(Ordering::Relaxed), 1));
            });
        });
        cx.sync_scope(|| cx.sync_scope(|| assert_eq!(entered.load(Ordering::Relaxed), 2)));
    }

    #[tokio::test]
    async fn native_completion_reclaims_common_tasks_without_canceling_the_wire_status() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let cx = context("owner");
            let shared = cx.shared_context();
            assert_eq!(shared.spawn_scoped(async { 42 }).await.unwrap(), 42);
            let (started, running) = tokio::sync::oneshot::channel();
            let work = shared.spawn_scoped(async move {
                let _ = started.send(());
                futures::future::pending::<()>().await;
            });
            running.await.unwrap();
            assert!(cx.finish(StreamStatus::default()));
            assert!(work.await.is_err());
            cx.tasks_reclaimed().await;
            assert!(cx.cancellation().is_none());
            assert!(cx.status().unwrap().error.is_none());
            let native_cancel = cx.cancelled();
            tokio::pin!(native_cancel);
            assert!(futures::poll!(&mut native_cancel).is_pending());
            cx.cancel_local("late reset");
            assert!(native_cancel.await.kitex_code().is_some());
            assert!(cx.status().unwrap().error.is_none());
            assert!(!cx.finish(StreamStatus::default()));
        })
        .await
        .unwrap();
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
