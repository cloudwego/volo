//! Owned state and extension hooks for an entire gRPC call.
//!
//! `scope` propagates context, while `spawn_scoped` also gives the framework
//! ownership of a child task. Bare `tokio::spawn` inherits neither guarantee.

pub(crate) mod lifecycle_body;
pub(crate) mod metadata;
mod production;
pub mod response;
pub(crate) mod shared;

use std::{
    collections::BTreeMap,
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use parking_lot::Mutex;
use tokio::{
    sync::Notify,
    task::{AbortHandle, JoinHandle},
};
use tokio_util::sync::CancellationToken;
use volo::context::{Extensions, MetaInfoScope, Role};

use crate::{Code, Status, context::Deadline, message::RpcMode, metadata::MetadataMap};

// Marks fully entered scopes. A hook may install a scope during admission,
// so identity alone cannot suppress a nested entry before that new scope runs.
tokio::task_local! { static ENTERED_SCOPE: (usize, u8); }

tokio::task_local! { pub(crate) static STREAM_CONTEXT: StreamContext; }

pub trait StreamHook: Send + Sync + 'static {
    fn on_open(&self, _cx: &StreamContext) -> Result<(), Status> {
        Ok(())
    }
    fn on_header(&self, _cx: &StreamContext, _header: &MetadataMap) {}
    fn on_send(&self, _cx: &StreamContext, _encoded_size: usize) {}
    fn on_recv(&self, _cx: &StreamContext, _decoded_size: usize) {}
    /// An outbound H2 window wait that resumed (including a peer reset).
    /// No clock is sampled on the unregistered path. Producer/queue waits are
    /// separate from H2 flow control and are not included here.
    fn on_flow_control(&self, _cx: &StreamContext, _wait: std::time::Duration) {}
    fn on_trailer(&self, _cx: &StreamContext, _trailer: &MetadataMap) {}
    fn on_recv_closed(&self, _cx: &StreamContext) {}
    fn on_io_error(&self, _cx: &StreamContext, _status: &Status) {}
    fn on_finish(&self, _cx: &StreamContext, _status: &StreamStatus) {}
}

/// The unregistered path has no hook allocation, virtual callback or hook lock.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopHooks;
impl StreamHook for NoopHooks {}

pub trait StreamHookFactory: Send + Sync + 'static {
    /// Static admission before context allocation. The default observes every
    /// call; payload-specific integrations can leave old services unchanged.
    fn enabled(&self, _method: Option<&crate::message::MethodDescriptor>) -> bool {
        true
    }
    fn create(&self, cx: &StreamContext) -> Arc<dyn StreamHook>;
}

/// Installs a framework's task-local scope during each poll of a call, stream,
/// or managed child task. Implementations must invoke `operation` exactly once
/// and must not retain it. Hooks always receive the explicit context.
pub trait StreamScope: Send + Sync + 'static {
    fn enter(&self, cx: &StreamContext, operation: &mut dyn FnMut());
}

impl<F, H> StreamHookFactory for F
where
    F: Fn(&StreamContext) -> H + Send + Sync + 'static,
    H: StreamHook,
{
    fn create(&self, cx: &StreamContext) -> Arc<dyn StreamHook> {
        Arc::new(self(cx))
    }
}

#[derive(Clone)]
pub(crate) struct HookFactory(pub Arc<dyn StreamHookFactory>);
impl HookFactory {
    pub(crate) fn accepts(
        &self,
        method: Option<&crate::message::MethodDescriptor>,
    ) -> Result<bool, Status> {
        catch_unwind(AssertUnwindSafe(|| self.0.enabled(method)))
            .map_err(|_| Status::internal("stream hook admission panicked"))
    }
    pub(crate) fn append(self, next: Self) -> Self {
        Self(Arc::new(ChainedFactory(self, next)))
    }
}
struct ChainedFactory(HookFactory, HookFactory);
impl StreamHookFactory for ChainedFactory {
    fn enabled(&self, method: Option<&crate::message::MethodDescriptor>) -> bool {
        self.0.0.enabled(method) || self.1.0.enabled(method)
    }
    fn create(&self, cx: &StreamContext) -> Arc<dyn StreamHook> {
        let method = cx.with_extensions(|extensions| {
            extensions
                .get::<crate::message::MethodDescriptor>()
                .copied()
        });
        match (
            self.0.0.enabled(method.as_ref()),
            self.1.0.enabled(method.as_ref()),
        ) {
            (true, true) => Arc::new(ChainedHook {
                first: HookBranch::new(self.0.0.create(cx)),
                second: HookBranch::new(self.1.0.create(cx)),
            }),
            (true, false) => self.0.0.create(cx),
            (false, true) => self.1.0.create(cx),
            (false, false) => Arc::new(NoopHooks),
        }
    }
}
struct HookBranch {
    inner: Arc<dyn StreamHook>,
    disabled: AtomicBool,
}
impl HookBranch {
    fn new(inner: Arc<dyn StreamHook>) -> Self {
        Self {
            inner,
            disabled: AtomicBool::new(false),
        }
    }
    fn call<R>(&self, callback: impl FnOnce(&dyn StreamHook) -> R) -> Option<R> {
        if self.disabled.load(Ordering::Relaxed) {
            return None;
        }
        match catch_unwind(AssertUnwindSafe(|| callback(&*self.inner))) {
            Ok(value) => Some(value),
            Err(_) => {
                self.disabled.store(true, Ordering::Relaxed);
                tracing::error!("gRPC observer panicked and was disabled");
                None
            }
        }
    }
}
struct ChainedHook {
    first: HookBranch,
    second: HookBranch,
}
impl StreamHook for ChainedHook {
    fn on_open(&self, cx: &StreamContext) -> Result<(), Status> {
        self.first
            .call(|hook| hook.on_open(cx))
            .unwrap_or_else(|| Err(Status::internal("stream open hook panicked")))?;
        self.second
            .call(|hook| hook.on_open(cx))
            .unwrap_or_else(|| Err(Status::internal("stream open hook panicked")))
    }
    fn on_header(&self, cx: &StreamContext, metadata: &MetadataMap) {
        self.first.call(|hook| hook.on_header(cx, metadata));
        self.second.call(|hook| hook.on_header(cx, metadata));
    }
    fn on_send(&self, cx: &StreamContext, size: usize) {
        self.first.call(|hook| hook.on_send(cx, size));
        self.second.call(|hook| hook.on_send(cx, size));
    }
    fn on_recv(&self, cx: &StreamContext, size: usize) {
        self.first.call(|hook| hook.on_recv(cx, size));
        self.second.call(|hook| hook.on_recv(cx, size));
    }
    fn on_flow_control(&self, cx: &StreamContext, wait: std::time::Duration) {
        self.first.call(|hook| hook.on_flow_control(cx, wait));
        self.second.call(|hook| hook.on_flow_control(cx, wait));
    }
    fn on_trailer(&self, cx: &StreamContext, metadata: &MetadataMap) {
        self.first.call(|hook| hook.on_trailer(cx, metadata));
        self.second.call(|hook| hook.on_trailer(cx, metadata));
    }
    fn on_recv_closed(&self, cx: &StreamContext) {
        self.first.call(|hook| hook.on_recv_closed(cx));
        self.second.call(|hook| hook.on_recv_closed(cx));
    }
    fn on_io_error(&self, cx: &StreamContext, status: &Status) {
        self.first.call(|hook| hook.on_io_error(cx, status));
        self.second.call(|hook| hook.on_io_error(cx, status));
    }
    fn on_finish(&self, cx: &StreamContext, status: &StreamStatus) {
        self.first.call(|hook| hook.on_finish(cx, status));
        self.second.call(|hook| hook.on_finish(cx, status));
    }
}

#[derive(Clone, Default)]
struct ResponseMetadata {
    headers: MetadataMap,
    trailers: MetadataMap,
}
impl std::fmt::Debug for HookFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HookFactory { .. }")
    }
}

#[derive(Default)]
struct Hook {
    inner: OnceLock<Arc<dyn StreamHook>>,
    common: OnceLock<shared::CommonHook>,
    disabled: AtomicBool,
    // High bit closes admission; lower bits count callbacks already running.
    // The last one publishes finish. This also permits a hook to cancel its
    // own RPC without deadlocking on a callback mutex.
    events: AtomicUsize,
}

const HOOK_CLOSED: usize = 1 << (usize::BITS - 1);

#[derive(Clone, Debug)]
pub struct StreamStatus {
    pub status: Status,
    pub declared_exception: bool,
    pub sent_messages: usize,
    pub sent_bytes: usize,
    pub received_messages: usize,
    pub received_bytes: usize,
}

struct Inner {
    id: usize,
    method: faststr::FastStr,
    mode: Option<RpcMode>,
    role: Role,
    deadline: Option<Deadline>,
    token: CancellationToken,
    terminal: OnceLock<StreamStatus>,
    request_metadata: OnceLock<MetadataMap>,
    header: OnceLock<MetadataMap>,
    trailer: OnceLock<MetadataMap>,
    response_header_closed: AtomicBool,
    response_trailer_closed: AtomicBool,
    hook: OnceLock<Hook>,
    scope: OnceLock<Arc<dyn StreamScope>>,
    metainfo: MetaInfoScope,
    extensions: Mutex<Extensions>,
    middleware: Option<volo::streaming::ServerMiddleware>,
    common_hooks: OnceLock<volo::streaming::CallHooks>,
    common_scope: OnceLock<volo::streaming::SharedScope>,
    children: Mutex<BTreeMap<usize, AbortHandle>>,
    child_sequence: AtomicUsize,
    children_changed: Notify,
    timer: Mutex<Option<AbortHandle>>,
    sent_messages: AtomicUsize,
    sent_bytes: AtomicUsize,
    received_messages: AtomicUsize,
    received_bytes: AtomicUsize,
    declared_exception: AtomicBool,
    decoder_owned: AtomicBool,
    production: Mutex<Option<production::Production>>,
    production_changed: Notify,
    h2_owned: AtomicBool,
}

/// Shared authority carried by request/response extensions and stream handles.
#[derive(Clone)]
pub struct StreamContext(Arc<Inner>);

impl std::fmt::Debug for StreamContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamContext")
            .field("id", &self.0.id)
            .field("method", &self.0.method)
            .field("mode", &self.0.mode)
            .field("role", &self.0.role)
            .field("deadline", &self.0.deadline)
            .field("finished", &self.0.terminal.get().is_some())
            .finish_non_exhaustive()
    }
}

impl StreamContext {
    /// Returns the public streaming view over this call's existing authority.
    pub fn shared_context(&self) -> volo::streaming::StreamContext {
        volo::streaming::StreamContext::from_driver(self.0.clone())
    }

    pub(crate) fn set_request_metadata(&self, metadata: MetadataMap) {
        let _ = self.0.request_metadata.set(metadata);
    }

    pub fn new(
        method: impl Into<faststr::FastStr>,
        mode: Option<RpcMode>,
        role: Role,
        deadline: Option<Deadline>,
    ) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(1);
        let parent = Self::current();
        let token = parent
            .as_ref()
            .map(|parent| parent.0.token.child_token())
            .unwrap_or_default();
        let deadline = match (deadline, parent.and_then(|parent| parent.deadline())) {
            (Some(a), Some(b)) => Some(if a.instant() < b.instant() { a } else { b }),
            (a, b) => a.or(b),
        };
        Self(Arc::new(Inner {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            method: method.into(),
            mode,
            role,
            deadline,
            token,
            terminal: OnceLock::new(),
            request_metadata: OnceLock::new(),
            header: OnceLock::new(),
            trailer: OnceLock::new(),
            response_header_closed: AtomicBool::new(false),
            response_trailer_closed: AtomicBool::new(false),
            hook: OnceLock::new(),
            scope: OnceLock::new(),
            metainfo: MetaInfoScope::capture(),
            extensions: Mutex::default(),
            middleware: (role == Role::Server)
                .then(volo::streaming::ServerMiddleware::current)
                .flatten(),
            common_hooks: OnceLock::new(),
            common_scope: OnceLock::new(),
            children: Mutex::default(),
            child_sequence: AtomicUsize::new(1),
            children_changed: Notify::new(),
            timer: Mutex::default(),
            sent_messages: AtomicUsize::new(0),
            sent_bytes: AtomicUsize::new(0),
            received_messages: AtomicUsize::new(0),
            received_bytes: AtomicUsize::new(0),
            declared_exception: AtomicBool::new(false),
            decoder_owned: AtomicBool::new(false),
            production: Mutex::new(None),
            production_changed: Notify::new(),
            h2_owned: AtomicBool::new(false),
        }))
    }

    pub fn current() -> Option<Self> {
        STREAM_CONTEXT.try_with(Clone::clone).ok()
    }
    pub fn id(&self) -> usize {
        self.0.id
    }
    pub fn method(&self) -> &str {
        &self.0.method
    }
    pub fn mode(&self) -> Option<RpcMode> {
        self.0.mode
    }
    pub fn role(&self) -> Role {
        self.0.role
    }
    pub fn deadline(&self) -> Option<Deadline> {
        self.0.deadline
    }
    pub fn status(&self) -> Option<&StreamStatus> {
        self.0.terminal.get()
    }
    pub fn initial_metadata(&self) -> Option<&MetadataMap> {
        self.0.header.get()
    }
    pub fn trailers(&self) -> Option<&MetadataMap> {
        self.0.trailer.get()
    }
    pub fn with_metainfo<R>(&self, read: impl FnOnce(&metainfo::MetaInfo) -> R) -> R {
        self.0.metainfo.with(read)
    }
    pub fn update_metainfo<R>(&self, update: impl FnOnce(&mut metainfo::MetaInfo) -> R) -> R {
        self.0.metainfo.update(update)
    }
    pub fn with_extensions<R>(&self, update: impl FnOnce(&mut Extensions) -> R) -> R {
        update(&mut self.0.extensions.lock())
    }

    /// Merge response metadata before transport takes the initial header snapshot. Explicit
    /// response metadata takes precedence over framework defaults.
    pub fn response_headers(&self, metadata: MetadataMap) -> Result<(), Status> {
        validate_application_metadata(&metadata)?;
        self.with_extensions(|extensions| {
            if self.role() != Role::Server
                || self.0.response_header_closed.load(Ordering::Acquire)
                || self.initial_metadata().is_some()
                || self.status().is_some()
                || self.production_result().is_some()
            {
                return Err(Status::failed_precondition(
                    "response headers already committed or not a server call",
                ));
            }
            if !extensions.contains::<ResponseMetadata>() {
                extensions.insert(ResponseMetadata::default());
            }
            extensions
                .get_mut::<ResponseMetadata>()
                .unwrap()
                .headers
                .merge(metadata);
            Ok(())
        })
    }

    /// Merge final application metadata before transport takes the trailer snapshot. The
    /// codec's grpc-status/message/details always take precedence.
    pub fn response_trailers(&self, metadata: MetadataMap) -> Result<(), Status> {
        validate_application_metadata(&metadata)?;
        self.with_extensions(|extensions| {
            if self.role() != Role::Server
                || self.0.response_trailer_closed.load(Ordering::Acquire)
                || self.status().is_some()
                || self.production_result().is_some()
            {
                return Err(Status::failed_precondition(
                    "response trailers already committed or not a server call",
                ));
            }
            if !extensions.contains::<ResponseMetadata>() {
                extensions.insert(ResponseMetadata::default());
            }
            extensions
                .get_mut::<ResponseMetadata>()
                .unwrap()
                .trailers
                .merge(metadata);
            Ok(())
        })
    }

    pub(crate) fn response_metadata(&self, trailer: bool) -> MetadataMap {
        self.with_extensions(|extensions| {
            // Close application updates in the same critical section as the snapshot.
            // A successful late update must never be omitted from the committed frame.
            if trailer {
                self.0
                    .response_trailer_closed
                    .store(true, Ordering::Release);
            } else {
                self.0.response_header_closed.store(true, Ordering::Release);
            }
            extensions
                .get_mut::<ResponseMetadata>()
                .map(|metadata| {
                    std::mem::take(if trailer {
                        &mut metadata.trailers
                    } else {
                        &mut metadata.headers
                    })
                })
                .unwrap_or_default()
        })
    }

    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        tokio::pin!(future);
        futures::future::poll_fn(|task| self.sync_scope(|| future.as_mut().poll(task))).await
    }
    pub fn sync_scope<R>(&self, operation: impl FnOnce() -> R) -> R {
        if self.0.is_entered() {
            return operation();
        }
        let state = self.0.scope_state();
        let shared = self.shared_context();
        let operation = || ENTERED_SCOPE.sync_scope(state, operation);
        shared.sync_scope_current(|| {
            STREAM_CONTEXT.sync_scope(self.clone(), || {
                let operation = || match self.0.common_scope.get() {
                    Some(scope) => scope.enter(&shared, operation),
                    None => operation(),
                };
                self.0.metainfo.sync_scope(|| match self.0.scope.get() {
                    None => operation(),
                    Some(scope) => {
                        let mut operation = Some(operation);
                        let mut result = None;
                        scope.enter(self, &mut || {
                            result = Some(operation
                                .take()
                                .expect("StreamScope invoked operation twice")(
                            ));
                        });
                        result.expect("StreamScope did not invoke operation")
                    }
                })
            })
        })
    }

    /// The first framework scope owns this call. Returns false if already set.
    pub fn set_scope(&self, scope: Arc<dyn StreamScope>) -> bool {
        self.0.scope.set(scope).is_ok()
    }

    pub fn cancel(&self) {
        self.finish(Status::cancelled("cancelled by caller"));
    }
    /// Fail this RPC and stop its owned IO/children. The first terminal status
    /// wins. An OK status cannot be used to bypass normal response validation.
    pub fn abort(&self, status: Status) {
        self.finish(if status.code() == Code::Ok {
            Status::internal("abort requires a failure status")
        } else {
            status
        });
    }
    pub async fn cancelled(&self) -> Status {
        self.0.token.cancelled().await;
        self.status()
            .map(|status| status.status.clone())
            .filter(|status| status.code() != Code::Ok)
            .unwrap_or_else(|| self.cancellation_status())
    }

    pub(crate) fn cancellation_status(&self) -> Status {
        self.0.cancellation_status()
    }

    /// Spawn a task owned by this RPC. Finishing/cancelling the RPC aborts it,
    /// even if it is pending without polling a request stream or cancellation.
    pub fn spawn_scoped<F>(&self, future: F) -> JoinHandle<Result<F::Output, Status>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let id = self.0.child_sequence.fetch_add(1, Ordering::Relaxed);
        let exited = Arc::new(AtomicBool::new(false));
        let owner = ChildOwner {
            cx: self.clone(),
            id,
            exited: exited.clone(),
        };
        let cx = self.clone();
        let (start, started) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(self.0.metainfo.task_scope(async move {
            let _owner = owner;
            if started.await.is_err() {
                return Err(Status::cancelled("RPC task never started"));
            }
            cx.scope(async {
                tokio::select! { biased;
                    status = cx.cancelled() => Err(status),
                    value = future => Ok(value),
                }
            })
            .await
        }));
        let abort = task.abort_handle();
        self.0.children.lock().insert(id, abort.clone());
        if exited.load(Ordering::Acquire) {
            self.0.children.lock().remove(&id);
            self.0.children_changed.notify_waiters();
        }
        if self.producer_error().is_some() || self.0.token.is_cancelled() {
            abort.abort();
        }
        let _ = start.send(());
        task
    }

    /// Wait for managed tasks to finish reclaiming their owned resources.
    pub async fn children_reclaimed(&self) {
        loop {
            let notified = self.0.children_changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.response_handler_reclaimed() && self.0.children.lock().is_empty() {
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn install_hook(&self, factory: Option<&HookFactory>) -> Result<(), Status> {
        if let Some(factory) = factory {
            let gate = self.0.hook.get_or_init(Hook::default);
            let mut events = gate.events.load(Ordering::Acquire);
            loop {
                if events & HOOK_CLOSED != 0 {
                    return Err(Status::cancelled("RPC finished before hook installation"));
                }
                match gate.events.compare_exchange_weak(
                    events,
                    events + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(actual) => events = actual,
                }
            }
            let _opening = HookEvent(self);
            let hook = catch_unwind(AssertUnwindSafe(|| factory.0.create(self)))
                .map_err(|_| Status::internal("stream hook factory panicked"))?;
            if gate.inner.set(hook.clone()).is_err() {
                return Err(Status::internal("stream hook already installed"));
            }
            let result =
                catch_unwind(AssertUnwindSafe(|| hook.on_open(self))).unwrap_or_else(|_| {
                    gate.disabled.store(true, Ordering::Relaxed);
                    Err(Status::internal("stream open hook panicked"))
                });
            if self
                .0
                .hook
                .get()
                .is_some_and(|hook| hook.disabled.load(Ordering::Relaxed))
            {
                return Err(Status::internal("stream open hook panicked"));
            }
            result?;
            if let Some(status) = self.status() {
                return Err(if status.status.code() == Code::Ok {
                    Status::cancelled("RPC finished during open hook")
                } else {
                    status.status.clone()
                });
            }
        }
        Ok(())
    }

    pub(crate) fn arm_deadline(&self) {
        // Native H2 owns a continuously polled deadline beside reset/cancel,
        // including while handlers, Body polls or flow control are pending.
        // Other entry points still need an independent timer task.
        if self.0.h2_owned.load(Ordering::Acquire) {
            return;
        }
        let Some(deadline) = self.deadline() else {
            return;
        };
        if self.status().is_some() {
            return;
        }
        let weak = Arc::downgrade(&self.0);
        let token = self.0.token.clone();
        let task = tokio::spawn(async move {
            tokio::select! { biased;
                _ = token.cancelled() => {},
                _ = tokio::time::sleep_until(deadline.instant()) => {
                    if let Some(owner) = weak.upgrade() { Self(owner).finish(Status::deadline_exceeded("timeout")); }
                }
            }
        });
        let abort = task.abort_handle();
        *self.0.timer.lock() = Some(abort.clone());
        if self.status().is_some() {
            abort.abort();
        }
    }

    fn with_hook(&self, mut call: impl FnMut(&dyn StreamHook)) {
        if let Some(hook) = self.0.hook.get() {
            let mut state = hook.events.load(Ordering::Acquire);
            loop {
                if state & HOOK_CLOSED != 0 {
                    return;
                }
                match hook.events.compare_exchange_weak(
                    state,
                    state + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(observed) => state = observed,
                }
            }
            let _event = HookEvent(self);
            if let Some(inner) = hook.inner.get()
                && !hook.disabled.load(Ordering::Relaxed)
                && catch_unwind(AssertUnwindSafe(|| call(&**inner))).is_err()
            {
                hook.disabled.store(true, Ordering::Relaxed);
                tracing::error!("gRPC stream hook panicked and was disabled");
            }
            if let Some(common) = hook.common.get() {
                call(common);
            }
        }
    }

    fn close_hook(&self) {
        if let Some(hook) = self.0.hook.get()
            && hook.events.fetch_or(HOOK_CLOSED, Ordering::AcqRel) == 0
        {
            self.notify_finish(hook);
        }
    }

    fn notify_finish(&self, hook: &Hook) {
        if let Some(inner) = hook.inner.get()
            && !hook.disabled.load(Ordering::Relaxed)
            && catch_unwind(AssertUnwindSafe(|| {
                inner.on_finish(self, self.status().unwrap())
            }))
            .is_err()
        {
            hook.disabled.store(true, Ordering::Relaxed);
            tracing::error!("gRPC stream finish hook panicked and was disabled");
        }
        if let Some(common) = hook.common.get() {
            common.on_finish(self, self.status().unwrap());
        }
    }

    pub(crate) fn on_header(&self, metadata: MetadataMap) {
        if self.role() == Role::Client
            && let Err(status) = metadata::read_backward(self, &metadata)
        {
            self.finish(status);
        }
        if self.0.header.set(metadata).is_ok() {
            self.with_hook(|hook| hook.on_header(self, self.0.header.get().unwrap()));
        }
    }
    pub(crate) fn on_trailer(&self, metadata: MetadataMap) {
        if self.role() == Role::Client
            && let Err(status) = metadata::read_backward(self, &metadata)
        {
            self.finish(status);
        }
        if self.0.trailer.set(metadata).is_ok() {
            self.with_hook(|hook| hook.on_trailer(self, self.0.trailer.get().unwrap()));
        }
    }
    pub(crate) fn on_send(&self, size: usize) {
        if self.status().is_some() {
            return;
        }
        self.0.sent_messages.fetch_add(1, Ordering::Relaxed);
        self.0.sent_bytes.fetch_add(size, Ordering::Relaxed);
        self.with_hook(|hook| hook.on_send(self, size));
    }
    pub(crate) fn on_recv(&self, size: usize) {
        if self.status().is_some() {
            return;
        }
        self.0.received_messages.fetch_add(1, Ordering::Relaxed);
        self.0.received_bytes.fetch_add(size, Ordering::Relaxed);
        self.with_hook(|hook| hook.on_recv(self, size));
    }
    pub(crate) fn on_io_error(&self, status: &Status) {
        self.with_hook(|hook| hook.on_io_error(self, status));
    }

    pub(crate) fn on_recv_closed(&self) {
        self.with_hook(|hook| hook.on_recv_closed(self));
    }

    pub(crate) fn has_hook(&self) -> bool {
        self.0.hook.get().is_some()
    }

    pub(crate) fn on_flow_control(&self, wait: std::time::Duration) {
        self.with_hook(|hook| hook.on_flow_control(self, wait));
    }

    pub(crate) fn check_recv_cardinality(&self) -> Result<(), Status> {
        let single = match self.role() {
            Role::Client => matches!(self.mode(), Some(RpcMode::Unary | RpcMode::ClientStreaming)),
            Role::Server => matches!(self.mode(), Some(RpcMode::Unary)),
        };
        if single && self.0.received_messages.load(Ordering::Relaxed) != 0 {
            return Err(match self.role() {
                Role::Client => Status::internal("Multiple unary response messages"),
                Role::Server => Status::invalid_argument("Multiple unary request messages"),
            });
        }
        Ok(())
    }

    pub(crate) fn validate_success_cardinality(&self) -> Result<(), Status> {
        if self.role() == Role::Client
            && matches!(self.mode(), Some(RpcMode::Unary | RpcMode::ClientStreaming))
            && self.0.received_messages.load(Ordering::Relaxed) != 1
        {
            return Err(Status::internal("Missing unary response message"));
        }
        Ok(())
    }

    pub(crate) fn record_declared_exception(&self) {
        self.0.declared_exception.store(true, Ordering::Relaxed);
    }

    pub(crate) fn attach_response_decoder(&self) {
        self.0.decoder_owned.store(true, Ordering::Release);
    }
    pub(crate) fn has_response_decoder(&self) -> bool {
        self.0.decoder_owned.load(Ordering::Acquire)
    }

    pub(crate) fn finish(&self, status: Status) -> bool {
        // Push commit and cancellation choose one terminal under the same gate.
        let production = self.0.production.lock();
        let won = self.record_terminal(status);
        drop(production);
        if won {
            self.finish_cleanup();
        }
        won
    }

    fn record_terminal(&self, status: Status) -> bool {
        let terminal = StreamStatus {
            declared_exception: status.code() == Code::Ok
                && self.0.declared_exception.load(Ordering::Relaxed),
            status,
            sent_messages: self.0.sent_messages.load(Ordering::Relaxed),
            sent_bytes: self.0.sent_bytes.load(Ordering::Relaxed),
            received_messages: self.0.received_messages.load(Ordering::Relaxed),
            received_bytes: self.0.received_bytes.load(Ordering::Relaxed),
        };
        self.0.terminal.set(terminal).is_ok()
    }

    fn finish_cleanup(&self) {
        self.0.token.cancel();
        if let Some(timer) = self.0.timer.lock().take() {
            timer.abort();
        }
        self.abort_response_handler();
        self.abort_children();
        self.close_hook();
    }

    fn abort_children(&self) {
        let children = self.0.children.lock().values().cloned().collect::<Vec<_>>();
        for child in children {
            child.abort();
        }
    }
}

fn validate_application_metadata(metadata: &MetadataMap) -> Result<(), Status> {
    for key in [
        "grpc-status",
        "grpc-message",
        "grpc-status-details-bin",
        "content-type",
        "grpc-encoding",
        "grpc-accept-encoding",
    ] {
        if metadata.contains_key(key) {
            return Err(Status::invalid_argument(
                "response metadata conflicts with a reserved gRPC field",
            ));
        }
    }
    Ok(())
}

struct HookEvent<'a>(&'a StreamContext);
impl Drop for HookEvent<'_> {
    fn drop(&mut self) {
        let hook = self.0.0.hook.get().unwrap();
        if hook.events.fetch_sub(1, Ordering::AcqRel) == HOOK_CLOSED | 1 {
            self.0.notify_finish(hook);
        }
    }
}

struct ChildOwner {
    cx: StreamContext,
    id: usize,
    exited: Arc<AtomicBool>,
}
impl Drop for ChildOwner {
    fn drop(&mut self) {
        self.exited.store(true, Ordering::Release);
        self.cx.0.children.lock().remove(&self.id);
        self.cx.0.children_changed.notify_waiters();
        if std::thread::panicking() {
            self.cx
                .finish(Status::internal("managed RPC task panicked"));
        }
    }
}

/// Transfers cancellation responsibility from the call future to its body.
pub(crate) struct CallGuard {
    pub(crate) context: StreamContext,
    armed: bool,
}

impl CallGuard {
    pub(crate) fn new(context: StreamContext) -> Self {
        Self {
            context,
            armed: true,
        }
    }
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for CallGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.context.finish(if std::thread::panicking() {
            Status::internal("RPC owner panicked")
        } else if self
            .context
            .deadline()
            .is_some_and(|deadline| deadline.is_expired())
        {
            Status::deadline_exceeded("timeout")
        } else {
            Status::cancelled("RPC owner dropped before final status")
        });
    }
}

impl Inner {
    pub(super) fn scope_state(&self) -> (usize, u8) {
        (
            self as *const Self as usize,
            u8::from(self.common_scope.get().is_some())
                | (u8::from(self.scope.get().is_some()) << 1),
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
    use super::*;

    #[test]
    fn finishing_during_open_waits_for_the_open_callback_to_return() {
        struct Observer {
            opened: Arc<AtomicBool>,
            finished: Arc<AtomicBool>,
        }
        impl StreamHook for Observer {
            fn on_open(&self, cx: &StreamContext) -> Result<(), Status> {
                cx.finish(Status::cancelled("finished during open"));
                assert!(!self.finished.load(Ordering::Acquire));
                self.opened.store(true, Ordering::Release);
                Ok(())
            }
            fn on_finish(&self, _: &StreamContext, _: &StreamStatus) {
                assert!(self.opened.load(Ordering::Acquire));
                self.finished.store(true, Ordering::Release);
            }
        }
        let opened = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let done = finished.clone();
        let factory = HookFactory(Arc::new(move |_: &StreamContext| Observer {
            opened: opened.clone(),
            finished: done.clone(),
        }));
        let cx = context(None);
        assert!(cx.install_hook(Some(&factory)).is_err());
        assert!(finished.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn shared_header_callback_defers_finish_even_with_native_events() {
        struct Observer(Arc<AtomicBool>);
        impl volo::streaming::StreamHook for Observer {
            fn on_header(
                &self,
                cx: &volo::streaming::StreamContext,
                _: volo::streaming::StreamDirection,
                _: &volo::streaming::Metadata,
            ) {
                cx.cancel(volo::streaming::RpcError::Protocol(
                    "header canceled".into(),
                ));
                assert!(!self.0.load(Ordering::Acquire));
            }
            fn on_finish(
                &self,
                _: &volo::streaming::StreamContext,
                _: &volo::streaming::StreamStatus,
            ) {
                self.0.store(true, Ordering::Release);
            }
        }
        let finished = Arc::new(AtomicBool::new(false));
        let done = finished.clone();
        let middleware = volo::streaming::ServerMiddleware::new(
            None,
            vec![Arc::new(move |_: &volo::streaming::StreamContext| {
                Observer(done.clone())
            })],
        );
        let cx = volo::streaming::ServerMiddleware::scope_optional(Some(middleware), async {
            context(None)
        })
        .await;
        assert!(cx.shared_context().open_hooks().is_err());
        assert!(finished.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn invoke_without_middleware_enters_both_current_contexts() {
        let cx = context(None);
        cx.shared_context()
            .invoke(|shared| async move {
                assert_eq!(StreamContext::current().unwrap().id() as u64, shared.id());
                assert_eq!(
                    volo::streaming::StreamContext::current().unwrap().id(),
                    shared.id()
                );
                Ok(())
            })
            .await
            .unwrap();
    }

    #[test]
    fn dropping_a_call_at_its_deadline_keeps_deadline_exceeded() {
        let cx = context(Some(Deadline::new(tokio::time::Instant::now())));
        drop(CallGuard::new(cx.clone()));
        assert_eq!(cx.status().unwrap().status.code(), Code::DeadlineExceeded);
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
        let cx = context(None);
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

    #[test]
    fn metadata_snapshots_close_updates_before_the_terminal_status_is_committed() {
        let cx = context(None);
        let mut original = MetadataMap::new();
        original.insert("x-header", "original".parse().unwrap());
        cx.response_headers(original.clone()).unwrap();
        let headers = cx.response_metadata(false);
        assert_eq!(headers.get("x-header").unwrap(), "original");
        assert!(cx.status().is_none());
        assert!(cx.initial_metadata().is_none());
        assert_eq!(
            cx.response_headers(original.clone()).unwrap_err().code(),
            Code::FailedPrecondition
        );
        cx.response_trailers(original.clone()).unwrap();
        let trailers = cx.response_metadata(true);
        assert_eq!(trailers.get("x-header").unwrap(), "original");
        assert!(cx.status().is_none());
        assert_eq!(
            cx.response_trailers(original).unwrap_err().code(),
            Code::FailedPrecondition
        );
    }

    #[tokio::test]
    async fn a_panicking_observer_does_not_disable_another_hooks_finish_cleanup() {
        struct Cleanup(Arc<AtomicBool>);
        impl StreamHook for Cleanup {
            fn on_finish(&self, _: &StreamContext, _: &StreamStatus) {
                self.0.store(true, Ordering::Release);
            }
        }
        struct Panicking;
        impl StreamHook for Panicking {
            fn on_send(&self, _: &StreamContext, _: usize) {
                panic!("fixture observer panic");
            }
        }
        let cleaned = Arc::new(AtomicBool::new(false));
        let observed = cleaned.clone();
        let hooks = HookFactory(Arc::new(move |_: &StreamContext| Cleanup(observed.clone())))
            .append(HookFactory(Arc::new(|_: &StreamContext| Panicking)));
        let cx = StreamContext::new("/test/Call", None, Role::Client, None);
        cx.install_hook(Some(&hooks)).unwrap();
        cx.on_send(1);
        cx.finish(Status::ok(""));
        assert!(cleaned.load(Ordering::Acquire));
    }
    fn context(deadline: Option<Deadline>) -> StreamContext {
        StreamContext::new(
            "/api.Service/Call",
            Some(RpcMode::Bidirectional),
            Role::Server,
            deadline,
        )
    }

    #[tokio::test]
    async fn finishing_aborts_owned_children_and_keeps_registration_until_reclaimed() {
        let cx = context(None);
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = cx.spawn_scoped(async {
            let _ = started.send(());
            std::future::pending::<()>().await
        });
        ready.await.unwrap();
        assert_eq!(cx.0.children.lock().len(), 1);
        cx.finish(Status::new(Code::Ok, ""));
        cx.children_reclaimed().await;
        assert!(task.await.unwrap_err().is_cancelled());
        let late = cx.spawn_scoped(async { panic!("a finished RPC cannot start a new child") });
        assert!(late.await.unwrap_err().is_cancelled());
        cx.children_reclaimed().await;
    }

    #[tokio::test]
    async fn tasklocal_and_managed_children_use_the_same_context() {
        use metainfo::Forward;
        let cx = context(None);
        cx.scope(async {
            metainfo::METAINFO.with(|slot| slot.borrow_mut().set_transient("key", "first"));
            tokio::task::yield_now().await;
            assert_eq!(
                cx.with_metainfo(|info| info.get_transient("key"))
                    .as_deref(),
                Some("first")
            );
            let id = cx.id();
            cx.spawn_scoped(async move {
                assert_eq!(StreamContext::current().unwrap().id(), id);
            })
            .await
            .unwrap()
            .unwrap();
        })
        .await;
    }

    #[tokio::test]
    async fn deadline_finishes_even_when_no_application_future_is_polled() {
        let cx = context(Some(Deadline::new(
            tokio::time::Instant::now() + std::time::Duration::from_millis(5),
        )));
        cx.arm_deadline();
        assert_eq!(cx.cancelled().await.code(), Code::DeadlineExceeded);
        assert_eq!(cx.status().unwrap().status.code(), Code::DeadlineExceeded);
        cx.children_reclaimed().await;
    }

    struct FinishCount(Arc<AtomicUsize>);
    impl StreamHook for FinishCount {
        fn on_finish(&self, _: &StreamContext, _: &StreamStatus) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    #[tokio::test]
    async fn guard_finish_and_cancel_publish_once() {
        let cx = context(None);
        let count = Arc::new(AtomicUsize::new(0));
        let captured = count.clone();
        cx.install_hook(Some(&HookFactory(Arc::new(move |_: &StreamContext| {
            FinishCount(captured.clone())
        }))))
        .unwrap();
        let guard = CallGuard::new(cx.clone());
        assert!(cx.finish(Status::new(Code::Ok, "")));
        cx.cancel();
        drop(guard);
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert_eq!(cx.status().unwrap().status.code(), Code::Ok);
    }

    struct CancelOnSend(Arc<Mutex<Vec<&'static str>>>);
    impl StreamHook for CancelOnSend {
        fn on_send(&self, cx: &StreamContext, _: usize) {
            self.0.lock().push("send-start");
            cx.cancel();
            self.0.lock().push("send-end");
        }
        fn on_finish(&self, _: &StreamContext, _: &StreamStatus) {
            self.0.lock().push("finish");
        }
    }

    #[tokio::test]
    async fn reentrant_cancel_defers_finish_until_the_callback_returns() {
        let cx = context(None);
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        cx.install_hook(Some(&HookFactory(Arc::new(move |_: &StreamContext| {
            CancelOnSend(captured.clone())
        }))))
        .unwrap();
        cx.on_send(3);
        cx.on_send(5);
        assert_eq!(*events.lock(), ["send-start", "send-end", "finish"]);
    }
}

#[cfg(test)]
mod common_observer_tests {
    use super::*;

    struct BlockingObserver {
        started: Arc<tokio::sync::Notify>,
        release: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
        active: Arc<AtomicBool>,
        finished: Arc<AtomicUsize>,
        violations: Arc<AtomicUsize>,
    }
    impl volo::streaming::StreamHook for BlockingObserver {
        fn on_send(&self, _: &volo::streaming::StreamContext, _: usize) {
            self.active.store(true, Ordering::Release);
            self.started.notify_one();
            self.release.lock().recv().unwrap();
            self.active.store(false, Ordering::Release);
        }
        fn on_finish(
            &self,
            cx: &volo::streaming::StreamContext,
            status: &volo::streaming::StreamStatus,
        ) {
            if self.active.load(Ordering::Acquire)
                || cx.status().is_none()
                || status.sent_messages != 1
            {
                self.violations.fetch_add(1, Ordering::Relaxed);
            }
            self.finished.fetch_add(1, Ordering::Release);
        }
    }
    #[tokio::test]
    async fn common_finish_waits_for_a_concurrent_callback_without_delaying_native_cancel() {
        use volo::streaming::{CancelCause, RpcError, ServerMiddleware};
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let started = Arc::new(tokio::sync::Notify::new());
            let (release, wait) = std::sync::mpsc::channel();
            let wait = Arc::new(Mutex::new(wait));
            let active = Arc::new(AtomicBool::new(false));
            let finished = Arc::new(AtomicUsize::new(0));
            let violations = Arc::new(AtomicUsize::new(0));
            let captured = (
                started.clone(),
                wait,
                active.clone(),
                finished.clone(),
                violations.clone(),
            );
            let factory = move |_: &volo::streaming::StreamContext| BlockingObserver {
                started: captured.0.clone(),
                release: captured.1.clone(),
                active: captured.2.clone(),
                finished: captured.3.clone(),
                violations: captured.4.clone(),
            };
            ServerMiddleware::scope_optional(
                Some(ServerMiddleware::new(None, vec![Arc::new(factory)])),
                async {
                    let cx = StreamContext::new(
                        "/Echo/Call",
                        Some(RpcMode::Bidirectional),
                        Role::Server,
                        None,
                    );
                    cx.shared_context().open_hooks().unwrap();
                    let sending = cx.clone();
                    let worker = tokio::task::spawn_blocking(move || sending.on_send(3));
                    started.notified().await;
                    cx.shared_context().cancel(RpcError::Canceled {
                        cause: CancelCause::Local,
                        code: 0,
                        message: "stop while callback runs".into(),
                        path: Vec::new(),
                    });
                    assert_eq!(cx.status().unwrap().status.code(), Code::Cancelled);
                    assert_eq!(finished.load(Ordering::Acquire), 0);
                    release.send(()).unwrap();
                    worker.await.unwrap();
                    assert_eq!(finished.load(Ordering::Acquire), 1);
                    assert_eq!(violations.load(Ordering::Relaxed), 0);
                    cx.on_send(9);
                    assert_eq!(finished.load(Ordering::Acquire), 1);
                },
            )
            .await;
        })
        .await
        .unwrap();
    }
}

impl crate::streaming::StreamContext {
    pub(crate) fn active_tasks(&self) -> usize {
        self.0.children.lock().len() + usize::from(!self.response_handler_reclaimed())
    }
}
