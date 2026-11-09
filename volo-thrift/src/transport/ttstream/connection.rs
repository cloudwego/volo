use std::{
    collections::HashSet,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
    },
    time::Instant,
};

use bytes::{Buf, Bytes};
use futures::FutureExt;
use parking_lot::Mutex;
use pilota::thrift::{ApplicationException, ApplicationExceptionKind};
use rustc_hash::FxHashMap;
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    sync::{Notify, mpsc},
};
use volo::context::Role;

use super::frame::{Frame, FrameRead, FrameTemplate, FrameType, read_frame_admitted};
pub use super::router::{StreamMethod, StreamRouter};
use crate::streaming::{
    CancelCause, HookChain, IoPhase, Metadata, OpenedStream, StreamConfig, StreamContext,
    StreamError, StreamHook,
    stream::{
        ControlObservation, Flow, Packet, ReceiveQueue, ReceiveReservation, RecvCore, SendCore,
        WriteCommand, decode_terminal, encode_terminal,
    },
};

static NEXT_ID: AtomicI32 = AtomicI32::new(0);
pub(crate) fn next_id() -> i32 {
    loop {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        if id != 0 {
            return id;
        }
    }
}
struct Entry {
    flow: Arc<Flow>,
}
#[derive(Default)]
struct Handlers {
    contexts: FxHashMap<i32, StreamContext>,
    // Includes canceled generations until their futures have actually exited.
    running: usize,
}
struct Shared {
    streams: Mutex<FxHashMap<i32, Entry>>,
    handlers: Option<Mutex<Handlers>>,
    quarantine: Mutex<HashSet<i32>>,
    recv_bytes: Arc<AtomicUsize>,
    send_bytes: Arc<AtomicUsize>,
    closed: AtomicBool,
    retiring: AtomicBool,
    reset_grace: Mutex<Option<(i32, Option<Instant>)>>,
    draining: AtomicBool,
    wake: Arc<Notify>,
    read_wake: Arc<Notify>,
    tx: mpsc::Sender<WriteCommand>,
    control: mpsc::Sender<WriteCommand>,
    config: Arc<StreamConfig>,
    hooks: Option<HookChain>,
    idle_since: Mutex<Option<Instant>>,
}
impl Shared {
    fn running_handlers(&self) -> usize {
        self.handlers.as_ref().map_or(0, |h| h.lock().running)
    }

    fn cancel_handlers(&self, error: StreamError) {
        let Some(handlers) = &self.handlers else {
            return;
        };
        let contexts: Vec<_> = handlers.lock().contexts.values().cloned().collect();
        for cx in contexts {
            cx.cancel(error.clone());
        }
    }

    // A wrapping sid may be reused after the previous stream is removed. Late
    // cleanup belongs to a concrete Flow generation, not merely its numeric id.
    fn remove_flow(&self, flow: &Arc<Flow>) -> bool {
        match self.streams.lock().entry(flow.cx.stream_id()) {
            std::collections::hash_map::Entry::Occupied(entry)
                if Arc::ptr_eq(&entry.get().flow, flow) =>
            {
                entry.remove();
                true
            }
            _ => false,
        }
    }
}
// Wire completion can precede handler completion. Keep a separate cancellation
// owner without retaining the stream's payloads or publishing another terminal.
struct HandlerGuard {
    shared: Arc<Shared>,
    cx: StreamContext,
}
impl HandlerGuard {
    fn register(shared: &Arc<Shared>, cx: &StreamContext) -> Result<Self, StreamError> {
        let mut handlers = shared.handlers.as_ref().expect("server handlers").lock();
        // Serialize registration with close's cancellation snapshot.
        if shared.closed.load(Ordering::Acquire) {
            return Err(StreamError::closed());
        }
        handlers.running += 1;
        let old = handlers.contexts.insert(cx.stream_id(), cx.clone());
        drop(handlers);
        if let Some(old) = old {
            // The old wire stream has completed, so sid reuse is legal. Cancel
            // its handler when the new generation takes over cancellation.
            old.cancel(StreamError::Canceled {
                cause: CancelCause::Local,
                code: 12012,
                message: "stream id reused after response completion".into(),
                path: Vec::new(),
            });
        }
        Ok(Self {
            shared: shared.clone(),
            cx: cx.clone(),
        })
    }
}
impl Drop for HandlerGuard {
    fn drop(&mut self) {
        let mut handlers = self
            .shared
            .handlers
            .as_ref()
            .expect("server handlers")
            .lock();
        handlers.running -= 1;
        if let std::collections::hash_map::Entry::Occupied(entry) =
            handlers.contexts.entry(self.cx.stream_id())
        {
            if Arc::ptr_eq(&entry.get().inner, &self.cx.inner) {
                entry.remove();
            }
        }
        drop(handlers);
        self.shared.wake.notify_waiters();
        self.shared.read_wake.notify_one();
    }
}
struct DriverGuard(Arc<Shared>);
impl Drop for DriverGuard {
    fn drop(&mut self) {
        close(&self.0, StreamError::closed());
    }
}
#[derive(Clone)]
pub struct Connection {
    shared: Arc<Shared>,
}
impl Connection {
    pub fn start<R, W>(
        reader: R,
        writer: W,
        config: StreamConfig,
        hooks: Option<HookChain>,
        router: Option<StreamRouter>,
    ) -> Result<Self, StreamError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (connection, reader) = Self::prepare(reader, writer, config, hooks, router)?;
        tokio::spawn(reader);
        Ok(connection)
    }
    fn prepare<R, W>(
        reader: R,
        writer: W,
        config: StreamConfig,
        hooks: Option<HookChain>,
        router: Option<StreamRouter>,
    ) -> Result<(Self, impl Future<Output = ()> + Send + 'static), StreamError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        if !cfg!(feature = "ttstream") {
            return Err(StreamError::Configuration(
                "ttstream feature is disabled".into(),
            ));
        }
        config.validate()?;
        if let Some(router) = &router {
            router.validate()?;
        }
        let hooks = hooks.filter(|hooks| !hooks.is_empty());
        let config = Arc::new(config);
        let (tx, rx) = mpsc::channel(config.write_queue_frames);
        let (control, controls) = mpsc::channel(config.control_queue_frames);
        let shared = Arc::new(Shared {
            streams: Mutex::default(),
            handlers: router.as_ref().map(|_| Mutex::default()),
            quarantine: Mutex::default(),
            recv_bytes: Arc::new(AtomicUsize::new(0)),
            send_bytes: Arc::new(AtomicUsize::new(0)),
            closed: AtomicBool::new(false),
            retiring: AtomicBool::new(false),
            reset_grace: Mutex::new(None),
            draining: AtomicBool::new(false),
            wake: Arc::new(Notify::new()),
            read_wake: Arc::new(Notify::new()),
            tx,
            control,
            config,
            hooks,
            idle_since: Mutex::new(Some(Instant::now())),
        });
        // Construct guards before either future can be dropped unpolled. A
        // canceled/panicking driver must terminate its sibling and all flows.
        let writing_shared = shared.clone();
        let writing_guard = DriverGuard(shared.clone());
        tokio::spawn(async move {
            let _guard = writing_guard;
            let mut writer = writer;
            let flush_timeout = writing_shared.config.shutdown_flush_timeout;
            let outcome = std::panic::AssertUnwindSafe(write_loop(
                &mut writer,
                rx,
                controls,
                writing_shared.clone(),
            ))
            .catch_unwind()
            .await;
            close(&writing_shared, StreamError::closed());
            // Dropping a socket signals EOF, but dropping a ShmIPC logical
            // stream only releases local buffers. Always close the transport
            // explicitly in this existing task, with a bound for custom I/O.
            let _ = tokio::time::timeout(flush_timeout, writer.shutdown()).await;
            if let Err(panic) = outcome {
                std::panic::resume_unwind(panic);
            }
        });
        let reading_shared = shared.clone();
        let reading_guard = DriverGuard(shared.clone());
        let reading = async move {
            let _guard = reading_guard;
            read_loop(reader, reading_shared, router).await;
        };
        Ok((Self { shared }, reading))
    }
    /// Runs the read loop in the existing server connection task, with one write task.
    pub(crate) async fn serve<R, W>(
        reader: R,
        writer: W,
        config: StreamConfig,
        hooks: Option<HookChain>,
        router: StreamRouter,
        shutdown: impl Future<Output = ()> + Send,
    ) -> Result<(), StreamError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let drain_timeout = config.drain_timeout;
        let flush_timeout = config.shutdown_flush_timeout;
        let retire_timeout = config.reset_retire_timeout;
        let (connection, reading) = Self::prepare(reader, writer, config, hooks, Some(router))?;
        tokio::pin!(reading);
        tokio::select! { _=&mut reading=>return Ok(()), _=shutdown=>connection.drain() }
        let deadline = tokio::time::sleep(drain_timeout);
        tokio::pin!(deadline);
        loop {
            let changed = connection.shared.wake.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if (connection.active_streams() == 0 && connection.shared.running_handlers() == 0)
                || connection.is_closed()
            {
                break;
            }
            tokio::select! {
                _=&mut reading=>return Ok(()),
                _=&mut deadline=>{ connection.abort_draining(); break; },
                _=changed=>{},
            }
        }
        let flush = tokio::time::timeout(flush_timeout, connection.flush_controls());
        tokio::pin!(flush);
        tokio::select! { _=&mut reading=>return Ok(()), _=&mut flush=>{} }
        connection.retire();
        tokio::select! {
            _=&mut reading=>{},
            _=tokio::time::sleep(retire_timeout)=>connection.close(),
        }
        connection.close();
        Ok(())
    }
    fn abort_draining(&self) {
        let flows: Vec<_> = self
            .shared
            .streams
            .lock()
            .drain()
            .map(|(_, entry)| entry.flow)
            .collect();
        for flow in flows {
            if flow.wire_finished() {
                flow.cx.cancel(StreamError::ServerDraining);
            } else {
                reset(
                    &self.shared,
                    &flow,
                    StreamError::ServerDraining,
                    ApplicationExceptionKind::INTERNAL_ERROR,
                );
            }
        }
        self.shared.cancel_handlers(StreamError::ServerDraining);
    }
    async fn flush_controls(&self) {
        let (flushed, done) = tokio::sync::oneshot::channel();
        if self
            .shared
            .control
            .send(WriteCommand {
                bytes: Bytes::new(),
                flow: None,
                finish: None,
                flushed: Some(flushed),
                control_observer: None,
                reset_id: None,
                _budget: None,
                _lease: None,
            })
            .await
            .is_ok()
        {
            let _ = done.await;
        }
    }
    pub async fn closed(&self) {
        wait_closed(&self.shared).await;
    }
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire) || self.shared.retiring.load(Ordering::Acquire)
    }
    pub(crate) fn retire(&self) {
        if self.shared.reset_grace.lock().is_none() {
            self.close();
        } else {
            self.shared.retiring.store(true, Ordering::Release);
            self.shared.wake.notify_waiters();
            self.shared.read_wake.notify_one();
        }
    }
    pub fn active_streams(&self) -> usize {
        self.shared.streams.lock().len()
    }
    pub fn drain(&self) {
        self.shared.draining.store(true, Ordering::Release);
        self.shared.wake.notify_waiters();
        self.shared.read_wake.notify_one();
    }
    pub fn close(&self) {
        close(&self.shared, StreamError::closed());
    }
    pub async fn open(
        &self,
        service: &str,
        method: &str,
        metadata: Metadata,
    ) -> Result<OpenedStream, StreamError> {
        if self.is_closed() {
            return Err(StreamError::closed());
        }
        let id = {
            let streams = self.shared.streams.lock();
            if streams.len() >= self.shared.config.max_streams {
                return Err(StreamError::ResourceExhausted {
                    scope: "active streams",
                    limit: self.shared.config.max_streams,
                    observed: streams.len() + 1,
                });
            }
            loop {
                let id = next_id();
                if !streams.contains_key(&id) && !self.shared.quarantine.lock().contains(&id) {
                    break id;
                }
            }
        };
        self.open_context(
            StreamContext::new(id, service, method, Role::Client),
            metadata,
        )
        .await
    }
    pub async fn open_context(
        &self,
        cx: StreamContext,
        metadata: Metadata,
    ) -> Result<OpenedStream, StreamError> {
        self.open_context_inner(cx, metadata, true).await
    }
    pub(crate) async fn open_prepared_context(
        &self,
        cx: StreamContext,
        metadata: Metadata,
    ) -> Result<OpenedStream, StreamError> {
        self.open_context_inner(cx, metadata, false).await
    }
    async fn open_context_inner(
        &self,
        mut cx: StreamContext,
        metadata: Metadata,
        run_hooks: bool,
    ) -> Result<OpenedStream, StreamError> {
        if self.is_closed() {
            return Err(StreamError::closed());
        }
        if cx.deadline().is_none() {
            cx.set_deadline(
                self.shared
                    .config
                    .deadline
                    .map(|duration| Instant::now() + duration),
            );
        }
        let id = cx.stream_id();
        let service = cx.service().to_owned();
        *cx.inner.driver_notify.lock() = Some(self.shared.read_wake.clone());
        let (flow, rx) = Flow::new(
            cx.clone(),
            self.shared.config.clone(),
            self.shared.hooks.clone(),
            true,
            self.shared.recv_bytes.clone(),
            self.shared.send_bytes.clone(),
        );
        if run_hooks {
            let mut request_metadata = metadata.clone();
            request_metadata.extend(cx.metadata());
            cx.update_metadata(|current| *current = request_metadata);
        }
        if let Some(hooks) = self.shared.hooks.as_ref().filter(|_| run_hooks) {
            if let Err(error) = hooks.on_open(&mut cx) {
                flow.finish(Some(error.clone()));
                return Err(error);
            }
        }
        let mut guard = PendingOpen {
            shared: self.shared.clone(),
            flow: flow.clone(),
            registered: false,
            completed: false,
        };
        let mut header = flow.frame(FrameType::Header, Bytes::new());
        header.metadata = metadata;
        header.metadata.extend(cx.metadata());
        cx.with_metainfo(|info| crate::streaming::export_metainfo(info, &mut header.metadata));
        header.metadata.insert(Bytes::from_static(b"isn"), service);
        let bytes = header.encode(&self.shared.config)?;
        let stream = opened(flow.clone(), rx, self.shared.tx.clone())?;
        {
            let mut streams = self.shared.streams.lock();
            if self.is_closed() {
                return Err(StreamError::closed());
            }
            if streams.len() >= self.shared.config.max_streams {
                return Err(StreamError::ResourceExhausted {
                    scope: "active streams",
                    limit: self.shared.config.max_streams,
                    observed: streams.len() + 1,
                });
            }
            if streams.contains_key(&id) || self.shared.quarantine.lock().contains(&id) {
                return Err(StreamError::Protocol("duplicate local stream id".into()));
            }
            streams.insert(id, Entry { flow: flow.clone() });
            guard.registered = true;
            *self.shared.idle_since.lock() = None;
            self.shared.read_wake.notify_one();
        }
        self.shared
            .tx
            .send(WriteCommand {
                bytes,
                flow: Some(flow),
                finish: None,
                flushed: None,
                control_observer: None,
                reset_id: None,
                _budget: None,
                _lease: None,
            })
            .await
            .map_err(|_| StreamError::closed())?;
        guard.completed = true;
        Ok(stream)
    }
}
/// A canceled/failed HEADER enqueue must not leave a hidden live stream in the map.
struct PendingOpen {
    shared: Arc<Shared>,
    flow: Arc<Flow>,
    registered: bool,
    completed: bool,
}
impl Drop for PendingOpen {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        if self.registered {
            self.shared.remove_flow(&self.flow);
        }
        self.flow
            .finish(Some(self.flow.cx.cancellation().unwrap_or_else(|| {
                StreamError::Canceled {
                    cause: CancelCause::Drop,
                    code: 12011,
                    message: "stream open was abandoned".into(),
                    path: Vec::new(),
                }
            })));
    }
}

fn opened(
    flow: Arc<Flow>,
    rx: Arc<ReceiveQueue>,
    tx: mpsc::Sender<WriteCommand>,
) -> Result<OpenedStream, StreamError> {
    let template = FrameTemplate::data(
        Bytes::copy_from_slice(flow.cx.method().as_bytes()),
        &flow.config,
    )?;
    Ok(OpenedStream {
        send: SendCore {
            flow: flow.clone(),
            tx,
            template,
        },
        recv: RecvCore::new(flow, rx),
    })
}
fn close(shared: &Shared, error: StreamError) {
    if shared.closed.swap(true, Ordering::AcqRel) {
        return;
    }
    let flows: Vec<_> = shared
        .streams
        .lock()
        .drain()
        .map(|(_, entry)| entry.flow)
        .collect();
    for flow in flows {
        // A short-pool lease closes the connection after its successful Trailer.
        // Do not rewrite that stream's already committed terminal status as EOF.
        if flow.cx.status().is_none() {
            let phase = match &error {
                StreamError::Transport { phase, .. } => *phase,
                StreamError::Protocol(_) | StreamError::ProtocolStatus { .. } => IoPhase::Decode,
                _ => IoPhase::Read,
            };
            let direction = if matches!(phase, IoPhase::Write | IoPhase::Flush | IoPhase::Encode) {
                crate::streaming::IoDirection::Send
            } else {
                crate::streaming::IoDirection::Recv
            };
            flow.io_error(direction, phase, None, error.clone());
            flow.cx.cancel(error.clone());
            flow.finish(Some(error.clone()));
        }
    }
    shared.cancel_handlers(error);
    shared.wake.notify_waiters();
    shared.read_wake.notify_one();
}
fn reset(shared: &Shared, flow: &Arc<Flow>, error: StreamError, kind: ApplicationExceptionKind) {
    if matches!(error, StreamError::ResourceExhausted { .. }) {
        flow.cx
            .inner
            .resources
            .buffer_limit_resets
            .fetch_add(1, Ordering::Relaxed);
    }
    // Hold the pool lease through the control flush. In short mode releasing it
    // before enqueue would close the socket and silently lose this RST.
    let lease = flow.lease.lock().take();
    if !shared.config.reset_retire_timeout.is_zero() {
        *shared.reset_grace.lock() = Some((flow.cx.stream_id(), None));
    }
    let control_observer = flow
        .hooks
        .as_ref()
        .map(|_| ControlObservation::flow(flow.clone()));
    flow.cx.cancel(error.clone());
    let error = flow.cx.cancellation().unwrap_or(error);
    flow.finish(Some(error.clone()));
    shared.wake.notify_waiters();
    write_reset(shared, &flow.cx, control_observer, error, kind, lease);
}
fn write_reset(
    shared: &Shared,
    cx: &StreamContext,
    control_observer: Option<ControlObservation>,
    error: StreamError,
    kind: ApplicationExceptionKind,
    lease: Option<Box<dyn Send + Sync>>,
) {
    if !shared.config.reset_retire_timeout.is_zero() {
        *shared.reset_grace.lock() = Some((cx.stream_id(), None));
    }
    let mut frame = Frame::new(
        cx.stream_id(),
        FrameType::Reset,
        Bytes::copy_from_slice(cx.method().as_bytes()),
        Bytes::new(),
    );
    if let StreamError::Canceled { path, .. } = &error {
        let path = if path.is_empty() && cx.with_rpc_info(|rpc| rpc.role()) == Role::Server {
            cx.with_rpc_info(|rpc| rpc.callee().service_name().to_string())
        } else {
            path.join(",")
        };
        if !path.is_empty() {
            frame.metadata.insert(Bytes::from_static(b"ttscp"), path);
        }
    }
    let kind = match &error {
        StreamError::Canceled { code, .. } | StreamError::ProtocolStatus { code, .. } => {
            ApplicationExceptionKind::from_i32(*code)
        }
        StreamError::Application(error) => error.kind(),
        _ => kind,
    };
    let exception = StreamError::Application(ApplicationException::new(kind, error.to_string()));
    if encode_terminal(&exception, &mut frame).is_err() {
        close(shared, error);
        return;
    }
    match frame.encode(&shared.config) {
        Ok(bytes) => {
            if shared
                .control
                .try_send(WriteCommand {
                    bytes,
                    flow: None,
                    finish: None,
                    flushed: None,
                    control_observer,
                    reset_id: Some(cx.stream_id()),
                    _budget: None,
                    _lease: lease,
                })
                .is_err()
            {
                close(shared, StreamError::closed());
            }
        }
        Err(error) => close(shared, error),
    }
}
async fn read_loop<R: AsyncRead + Unpin>(
    mut reader: R,
    shared: Arc<Shared>,
    router: Option<StreamRouter>,
) {
    let mut next_check = maintain_streams(&shared, router.is_none());
    loop {
        if shared.closed.load(Ordering::Acquire) {
            return;
        }
        if !shared.config.max_read_pause.is_zero()
            && !shared.draining.load(Ordering::Acquire)
            && all_receivers_full(&shared)
        {
            let paused = Instant::now();
            let participants = shared
                .streams
                .lock()
                .values()
                .map(|entry| entry.flow.cx.clone())
                .collect::<Vec<_>>();
            let _ = tokio::time::timeout(shared.config.max_read_pause, async {
                loop {
                    let notified = shared.read_wake.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if shared.closed.load(Ordering::Acquire)
                        || shared.draining.load(Ordering::Acquire)
                        || !all_receivers_full(&shared)
                    {
                        break;
                    }
                    notified.await;
                }
            })
            .await;
            for cx in participants {
                cx.inner
                    .resources
                    .read_pauses
                    .fetch_add(1, Ordering::Relaxed);
                cx.inner.resources.read_pause_nanos.fetch_add(
                    crate::streaming::resources::nanos(paused.elapsed()),
                    Ordering::Relaxed,
                );
            }
        }
        let read = read_frame_admitted(&mut reader, &shared.config, |frame, len| {
            admit_payload(frame, len, &shared)
        });
        tokio::pin!(read);
        let result = loop {
            if shared.closed.load(Ordering::Acquire) {
                return;
            }
            let timer = async {
                match next_check {
                    Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                _=shared.read_wake.notified()=> { next_check = maintain_streams(&shared, router.is_none()); },
                _=timer=> { next_check = maintain_streams(&shared, router.is_none()); },
                result=&mut read=>break result,
            }
        };
        let (frame, budget) = match result {
            Ok(Some(FrameRead::Frame(frame, budget))) => (frame, budget),
            Ok(Some(FrameRead::Discarded)) => continue,
            Ok(None) => {
                close(&shared, StreamError::closed());
                return;
            }
            Err(error) => {
                close(&shared, error);
                return;
            }
        };
        if let Err(error) = dispatch(frame, budget, &shared, router.as_ref()) {
            close(&shared, error);
            return;
        }
    }
}
/// Lifecycle notifications and exact deadlines drive housekeeping. Idle pooled
/// connections do not create a periodic timer, and ordinary DATA consumption
/// does not scan every other stream on the connection.
fn maintain_streams(shared: &Shared, client: bool) -> Option<Instant> {
    let remove: Vec<_> = {
        let streams = shared.streams.lock();
        streams
            .iter()
            .filter_map(|(id, entry)| {
                if entry.flow.cx.status().is_some() {
                    // OpenBoundary may finish a failed outer layer after inner
                    // open has already submitted HEADER. Its dropped receiver
                    // still needs to notify the peer with the local RST.
                    let error = (!entry.flow.wire_finished())
                        .then(|| entry.flow.cx.cancellation())
                        .flatten();
                    return Some((*id, entry.flow.clone(), error));
                }
                let error = entry.flow.cx.cancellation().or_else(|| {
                    entry
                        .flow
                        .cx
                        .deadline()
                        .filter(|deadline| Instant::now() >= *deadline)
                        .map(|_| StreamError::Canceled {
                            cause: CancelCause::Deadline,
                            code: 12015,
                            message: "stream deadline exceeded".into(),
                            path: Vec::new(),
                        })
                });
                error.map(|error| (*id, entry.flow.clone(), Some(error)))
            })
            .collect()
    };
    for (_, flow, error) in remove {
        if !shared.remove_flow(&flow) {
            continue;
        }
        if let Some(error) = error {
            reset(
                shared,
                &flow,
                error,
                ApplicationExceptionKind::INTERNAL_ERROR,
            );
        }
    }
    let mut next = maintain_handlers(shared);
    if shared.retiring.load(Ordering::Acquire) {
        let grace = *shared.reset_grace.lock();
        match grace {
            None => {
                close(shared, StreamError::closed());
                return None;
            }
            Some((_, Some(deadline))) if Instant::now() >= deadline => {
                close(shared, StreamError::closed());
                return None;
            }
            Some((_, deadline)) => next = deadline,
        }
    }
    let streams = shared.streams.lock();
    if client {
        let mut idle = shared.idle_since.lock();
        if streams.is_empty() {
            let since = *idle.get_or_insert_with(Instant::now);
            let deadline = since + shared.config.pool_idle_timeout;
            if Instant::now() >= deadline {
                drop(idle);
                drop(streams);
                close(shared, StreamError::closed());
                return None;
            }
            next = Some(next.map_or(deadline, |current| current.min(deadline)));
        } else {
            *idle = None;
        }
    }
    for entry in streams.values() {
        if let Some(deadline) = entry.flow.cx.deadline() {
            next = Some(next.map_or(deadline, |current| current.min(deadline)));
        }
    }
    next
}

fn maintain_handlers(shared: &Shared) -> Option<Instant> {
    let handlers = shared.handlers.as_ref()?;
    let mut next = None;
    let mut expired = Vec::new();
    {
        let handlers = handlers.lock();
        for cx in handlers.contexts.values() {
            // Active wire streams retain the normal RST/deadline path above.
            if cx.status().is_none() || cx.cancellation().is_some() {
                continue;
            }
            if let Some(deadline) = cx.deadline() {
                if Instant::now() >= deadline {
                    expired.push(cx.clone());
                } else {
                    next = Some(next.map_or(deadline, |current: Instant| current.min(deadline)));
                }
            }
        }
    }
    for cx in expired {
        cx.cancel(StreamError::Canceled {
            cause: CancelCause::Deadline,
            code: 12015,
            message: "stream deadline exceeded".into(),
            path: Vec::new(),
        });
    }
    next
}

fn all_receivers_full(shared: &Shared) -> bool {
    let streams = shared.streams.lock();
    !streams.is_empty()
        && streams.values().all(|entry| {
            let flow = &entry.flow;
            !flow.recv_closed.load(Ordering::Acquire)
                && !flow.discard.load(Ordering::Acquire)
                && (flow.recv_bytes.load(Ordering::Relaxed) >= flow.config.recv_bytes
                    || flow.inbound.is_full())
        })
}
fn admit_payload(frame: &Frame, len: usize, shared: &Shared) -> Option<Option<ReceiveReservation>> {
    if frame.kind != FrameType::Data {
        return Some(None);
    }
    let flow = shared
        .streams
        .lock()
        .get(&frame.stream_id)
        .map(|entry| entry.flow.clone())?;
    if flow.recv_closed.load(Ordering::Acquire)
        || flow.discard.load(Ordering::Acquire)
        || flow.cx.status().is_some()
    {
        return None;
    }
    let full = flow.inbound.is_full();
    let reservation = if full {
        Err(StreamError::ResourceExhausted {
            scope: "stream messages",
            limit: flow.config.recv_messages,
            observed: flow.config.recv_messages + 1,
        })
    } else {
        ReceiveReservation::new(flow.clone(), len)
    };
    match reservation {
        Ok(reservation) => Some(Some(reservation)),
        Err(error) => {
            shared.remove_flow(&flow);
            reset(
                shared,
                &flow,
                error,
                ApplicationExceptionKind::INTERNAL_ERROR,
            );
            None
        }
    }
}

fn dispatch(
    frame: Frame,
    budget: Option<ReceiveReservation>,
    shared: &Arc<Shared>,
    router: Option<&StreamRouter>,
) -> Result<(), StreamError> {
    let id = frame.stream_id;
    if matches!(frame.kind, FrameType::Trailer | FrameType::Reset) {
        let mut grace = shared.reset_grace.lock();
        if grace.as_ref().is_some_and(|(sid, _)| *sid == id) {
            *grace = None;
            shared.wake.notify_waiters();
            shared.read_wake.notify_one();
        }
    }
    if frame.kind == FrameType::Header && router.is_some() {
        let old = shared.streams.lock().remove(&id);
        if let Some(old) = old.filter(|entry| !entry.flow.wire_finished()) {
            old.flow
                .cx
                .inner
                .resources
                .duplicate_sid_rejections
                .fetch_add(1, Ordering::Relaxed);
            shared.quarantine.lock().insert(id);
            reset(
                shared,
                &old.flow,
                StreamError::Protocol("duplicate active stream id".into()),
                ApplicationExceptionKind::PROTOCOL_ERROR,
            );
            if shared.quarantine.lock().len() > shared.config.max_quarantined_ids {
                return Err(StreamError::Protocol(
                    "stream id quarantine limit exceeded".into(),
                ));
            }
            return Ok(());
        }
        let service = frame
            .metadata
            .get("isn")
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default();
        let method = std::str::from_utf8(&frame.method)
            .map_err(|_| StreamError::Protocol("non-UTF8 method".into()))?;
        let handler = router.and_then(|r| r.find(&service, method));
        let resolved_service = handler
            .as_ref()
            .map(|(service, _)| service.as_str())
            .unwrap_or(&service);
        let mut cx = StreamContext::new(id, resolved_service, method, Role::Server);
        cx.set_deadline(
            shared
                .config
                .deadline
                .map(|duration| Instant::now() + duration),
        );
        if let Some((_, info)) = &handler {
            cx.set_mode(info.mode);
        }
        *cx.inner.driver_notify.lock() = Some(shared.read_wake.clone());
        cx.update_metadata(|m| m.extend(frame.metadata.clone()));
        cx.update_metainfo(|info| crate::streaming::import_metainfo(&frame.metadata, info));
        let rejection = if shared.quarantine.lock().contains(&id) {
            cx.inner
                .resources
                .duplicate_sid_rejections
                .fetch_add(1, Ordering::Relaxed);
            Some((
                StreamError::Protocol("duplicate active stream id".into()),
                ApplicationExceptionKind::PROTOCOL_ERROR,
            ))
        } else if shared.draining.load(Ordering::Acquire) {
            Some((
                StreamError::ServerDraining,
                ApplicationExceptionKind::INTERNAL_ERROR,
            ))
        } else if shared.streams.lock().len() >= shared.config.max_streams
            || shared.running_handlers() >= shared.config.max_streams
        {
            Some((
                StreamError::ResourceExhausted {
                    scope: "active streams",
                    limit: shared.config.max_streams,
                    observed: shared.config.max_streams + 1,
                },
                ApplicationExceptionKind::INTERNAL_ERROR,
            ))
        } else {
            None
        };
        if let Some((error, kind)) = rejection {
            // A rejected HEADER never allocates a Flow, receive queue or handler.
            if matches!(error, StreamError::ResourceExhausted { .. }) {
                cx.inner
                    .resources
                    .buffer_limit_resets
                    .fetch_add(1, Ordering::Relaxed);
            }
            let status = crate::streaming::StreamStatus {
                error: Some(error.clone()),
                header: cx.metadata(),
                resources: cx.resources(),
                ..Default::default()
            };
            cx.cancel(error.clone());
            cx.finish(status);
            let control_observer = shared
                .hooks
                .as_ref()
                .map(|hooks| ControlObservation::rejected(cx.clone(), hooks.clone()));
            write_reset(shared, &cx, control_observer, error, kind, None);
            return Ok(());
        }
        let (flow, rx) = Flow::new(
            cx.clone(),
            shared.config.clone(),
            shared.hooks.clone(),
            false,
            shared.recv_bytes.clone(),
            shared.send_bytes.clone(),
        );
        *flow.header.lock() = Some(frame.metadata.clone());
        let handler = handler.map(|(_, method)| method.handler);
        let hook_error = shared.hooks.as_ref().and_then(|h| h.on_open(&mut cx).err());
        let stream = opened(flow.clone(), rx, shared.tx.clone())?;
        let mut finish = SendCore {
            template: FrameTemplate::data(frame.method, &shared.config)?,
            flow: flow.clone(),
            tx: shared.tx.clone(),
        };
        let scopes = router
            .map(|router| router.scopes.clone())
            .unwrap_or_default();
        let handler_guard = HandlerGuard::register(shared, &cx)?;
        {
            let mut streams = shared.streams.lock();
            if shared.closed.load(Ordering::Acquire) {
                return Err(StreamError::closed());
            }
            streams.insert(id, Entry { flow: flow.clone() });
        }
        shared.read_wake.notify_one();
        tokio::spawn(async move {
            let _handler_guard = handler_guard;
            let result = if let Some(error) = hook_error {
                Err(error)
            } else if let Some(handler) = handler {
                use futures::FutureExt;
                let future = async {
                    let mut future = handler(cx.clone(), stream);
                    for scope in scopes.iter().rev() {
                        future = scope.scope(cx.clone(), future);
                    }
                    future.await
                };
                let future = std::panic::AssertUnwindSafe(cx.scope(future)).catch_unwind();
                tokio::select! {
                    result=future=>result.unwrap_or_else(|_| Err(StreamError::Application(ApplicationException::new(ApplicationExceptionKind::INTERNAL_ERROR, "stream handler panicked")))),
                    error=cx.cancelled()=>Err(error),
                }
            } else {
                cx.inner
                    .resources
                    .route_failures
                    .fetch_add(1, Ordering::Relaxed);
                Err(StreamError::Application(ApplicationException::new(
                    ApplicationExceptionKind::UNKNOWN_METHOD,
                    format!("unknown service or method: {service}/{}", cx.method()),
                )))
            };
            if let Err(error) = finish.close(result.err()).await {
                flow.finish(Some(error));
            }
        });
        return Ok(());
    }
    let flow = shared
        .streams
        .lock()
        .get(&id)
        .map(|entry| entry.flow.clone());
    let Some(flow) = flow else {
        if frame.kind == FrameType::Reset {
            let cx = shared
                .handlers
                .as_ref()
                .and_then(|handlers| handlers.lock().contexts.get(&id).cloned());
            if let Some(cx) = cx {
                cx.cancel(peer_reset(&frame, false));
            }
        }
        return Ok(());
    };
    match frame.kind {
        FrameType::Header => {
            if flow.header.lock().is_some() {
                return Err(StreamError::ProtocolStatus {
                    code: 12002,
                    message: format!("stream[{id}] already set header"),
                });
            }
            *flow.header.lock() = Some(frame.metadata.clone());
            flow.header_notify.notify_waiters();
            if let Some(h) = &flow.hooks {
                h.on_header(&mut flow.cx.clone(), &frame.metadata);
            }
        }
        FrameType::Meta => {
            flow.cx
                .update_metadata(|m| m.extend(frame.metadata.clone()));
            if let Some(h) = &flow.hooks {
                h.on_meta(&mut flow.cx.clone(), &frame.metadata);
            }
        }
        FrameType::Data => {
            if flow.recv_closed.load(Ordering::Acquire) || flow.discard.load(Ordering::Acquire) {
                return Ok(());
            }
            let Some(budget) = budget else {
                return Ok(());
            };
            let packet = Packet {
                bytes: frame.payload,
                _budget: budget,
            };
            if matches!(
                flow.inbound.push(packet),
                Err(crate::streaming::stream::ReceivePushError::Full)
            ) && !flow.discard.load(Ordering::Acquire)
                && flow.cx.status().is_none()
            {
                shared.remove_flow(&flow);
                reset(
                    shared,
                    &flow,
                    StreamError::ResourceExhausted {
                        scope: "stream messages",
                        limit: flow.config.recv_messages,
                        observed: flow.config.recv_messages + 1,
                    },
                    ApplicationExceptionKind::INTERNAL_ERROR,
                );
            }
        }
        FrameType::Trailer => {
            if flow.recv_closed.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
            *flow.trailer.lock() = frame.metadata.clone();
            let error = match decode_terminal(&frame) {
                Ok(error) => error,
                Err(error) => Some(error),
            };
            if flow.client {
                flow.finish(error);
                shared.remove_flow(&flow);
            } else {
                *flow.recv_error.lock() = error;
                flow.inbound.close(false);
            }
        }
        FrameType::Reset => {
            let error = peer_reset(&frame, flow.client);
            flow.cx.cancel(error.clone());
            flow.finish(Some(error));
            shared.remove_flow(&flow);
        }
    }
    Ok(())
}
fn peer_reset(frame: &Frame, client: bool) -> StreamError {
    let (code, message) = match crate::streaming::decode_exception(frame.payload.clone()) {
        Ok(error) => (error.kind().as_i32(), error.message().to_string()),
        Err(_) => (13, "rst".into()),
    };
    let path = frame
        .metadata
        .get("ttscp")
        .map(|s| {
            String::from_utf8_lossy(s)
                .split(',')
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    StreamError::Canceled {
        cause: if client {
            CancelCause::Downstream
        } else {
            CancelCause::Upstream
        },
        code,
        message,
        path,
    }
}
async fn wait_closed(shared: &Shared) {
    loop {
        let notified = shared.wake.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if shared.closed.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut rx: mpsc::Receiver<WriteCommand>,
    mut controls: mpsc::Receiver<WriteCommand>,
    shared: Arc<Shared>,
) {
    loop {
        if shared.closed.load(Ordering::Acquire) {
            return;
        }
        let command = tokio::select! {biased;command=controls.recv()=>command,command=rx.recv()=>command,_=wait_closed(&shared)=>return};
        let Some(command) = command else {
            return;
        };
        if command
            .flow
            .as_ref()
            .is_some_and(|flow| flow.cx.status().is_some())
        {
            continue;
        }
        let mut batch = vec![command];
        let mut size = batch[0].bytes.len();
        while batch.len() < shared.config.batch_frames && size < shared.config.batch_bytes {
            match rx.try_recv() {
                Ok(command) => {
                    if command
                        .flow
                        .as_ref()
                        .is_some_and(|flow| flow.cx.status().is_some())
                    {
                        continue;
                    }
                    size += command.bytes.len();
                    batch.push(command);
                }
                Err(_) => break,
            }
        }
        let mut partial = None;
        loop {
            let changed = shared.wake.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if shared.closed.load(Ordering::Acquire) {
                return;
            }
            for (index, command) in batch.iter_mut().enumerate() {
                if partial != Some(index)
                    && !command.bytes.is_empty()
                    && command.flow.as_ref().is_some_and(|flow| {
                        flow.cx.status().is_some() || flow.cx.cancellation().is_some()
                    })
                {
                    // An unstarted frame is safe to discard. Once any bytes of a
                    // frame were accepted, finish that frame to preserve framing.
                    command.bytes = Bytes::new();
                    command.finish = None;
                    command.flow = None;
                    command._budget = None;
                }
            }
            let mut slices: [std::io::IoSlice<'_>; 64] =
                std::array::from_fn(|_| std::io::IoSlice::new(&[]));
            let mut count = 0;
            for command in batch
                .iter()
                .filter(|command| !command.bytes.is_empty())
                .take(slices.len())
            {
                slices[count] = std::io::IoSlice::new(&command.bytes);
                count += 1;
            }
            if count == 0 {
                break;
            }
            let result = tokio::select! {biased; _=&mut changed=>continue, result=writer.write_vectored(&slices[..count])=>result};
            match result {
                Ok(0) => {
                    close(
                        &shared,
                        StreamError::transport(
                            std::io::Error::new(
                                std::io::ErrorKind::WriteZero,
                                "TTStream write made no progress",
                            ),
                            IoPhase::Write,
                            true,
                        ),
                    );
                    return;
                }
                Ok(mut written) => {
                    for (index, command) in batch.iter_mut().enumerate() {
                        if written == 0 {
                            break;
                        }
                        let count = written.min(command.bytes.len());
                        command.bytes.advance(count);
                        written -= count;
                        partial = (!command.bytes.is_empty()).then_some(index);
                    }
                }
                Err(error) => {
                    close(&shared, StreamError::transport(error, IoPhase::Write, true));
                    return;
                }
            }
        }
        let flushed =
            tokio::select! { result=writer.flush()=>result, _=wait_closed(&shared)=>return };
        if let Err(error) = flushed {
            close(&shared, StreamError::transport(error, IoPhase::Flush, true));
            return;
        }
        for command in batch {
            if let Some(id) = command.reset_id {
                let mut grace = shared.reset_grace.lock();
                if let Some((_, deadline)) = grace.as_mut().filter(|(sid, _)| *sid == id) {
                    *deadline = Some(Instant::now() + shared.config.reset_retire_timeout);
                    shared.wake.notify_waiters();
                    shared.read_wake.notify_one();
                }
            }
            if let Some(mut observation) = command.control_observer {
                observation.flushed();
            }
            if let Some(flushed) = command.flushed {
                let _ = flushed.send(());
            }
            if let (Some(flow), Some(error)) = (command.flow, command.finish) {
                flow.finish(error);
                shared.wake.notify_waiters();
                shared.read_wake.notify_one();
            }
        }
    }
}

#[cfg(all(test, feature = "ttstream"))]
mod tests {
    use std::pin::Pin;

    use super::{super::frame::read_frame, *};

    fn echo_router() -> StreamRouter {
        let mut router = StreamRouter::new();
        router
            .add("Echo", "chat", |_, mut stream| async move {
                while let Some(bytes) = stream.recv_raw().await? {
                    stream.send_raw(bytes).await?;
                }
                Ok(())
            })
            .unwrap();
        router
    }

    fn pair(router: StreamRouter) -> (Connection, Connection) {
        let (client, server) = tokio::io::duplex(1024);
        let (cr, cw) = tokio::io::split(client);
        let (sr, sw) = tokio::io::split(server);
        (
            Connection::start(cr, cw, StreamConfig::default(), None, None).unwrap(),
            Connection::start(sr, sw, StreamConfig::default(), None, Some(router)).unwrap(),
        )
    }

    #[tokio::test]
    async fn connection_close_shuts_down_writer_and_bounds_pending_shutdown() {
        use std::task::{Context, Poll};
        struct Writer {
            calls: Arc<AtomicUsize>,
            dropped: Arc<Notify>,
            pending: bool,
            panic_write: bool,
        }
        impl AsyncWrite for Writer {
            fn poll_write(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                bytes: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                assert!(!self.panic_write, "custom writer panic");
                Poll::Ready(Ok(bytes.len()))
            }
            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
            fn poll_shutdown(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                if self.pending {
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(()))
                }
            }
        }
        impl Drop for Writer {
            fn drop(&mut self) {
                self.dropped.notify_one();
            }
        }
        for (pending, panic_write) in [(false, false), (true, false), (false, true), (true, true)] {
            let (reader, _peer) = tokio::io::duplex(64);
            let calls = Arc::new(AtomicUsize::new(0));
            let dropped = Arc::new(Notify::new());
            let connection = Connection::start(
                reader,
                Writer {
                    calls: calls.clone(),
                    dropped: dropped.clone(),
                    pending,
                    panic_write,
                },
                StreamConfig {
                    shutdown_flush_timeout: std::time::Duration::from_millis(20),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
            if panic_write {
                let _ = connection.open("S", "M", Metadata::default()).await;
            } else {
                connection.close();
            }
            tokio::time::timeout(std::time::Duration::from_millis(250), dropped.notified())
                .await
                .expect("shutdown must be bounded");
            assert!(
                calls.load(Ordering::Relaxed) > 0,
                "connection close must invoke writer shutdown; Drop is insufficient for logical \
                 streams"
            );
        }
    }

    #[tokio::test]
    async fn dropping_unpolled_read_driver_closes_its_writer_and_connection() {
        let (socket, _peer) = tokio::io::duplex(1024);
        let (reader, writer) = tokio::io::split(socket);
        let (connection, reading) =
            Connection::prepare(reader, writer, StreamConfig::default(), None, None).unwrap();
        drop(reading);
        assert!(
            connection.is_closed(),
            "dropped server driver must release the separately spawned writer"
        );
        connection.close();
    }

    #[tokio::test]
    async fn stale_generation_cleanup_cannot_remove_reused_sid() {
        let (client, server) = pair(echo_router());
        let make_flow = || {
            Flow::new(
                StreamContext::new(-1, "Echo", "chat", Role::Server),
                server.shared.config.clone(),
                None,
                false,
                server.shared.recv_bytes.clone(),
                server.shared.send_bytes.clone(),
            )
            .0
        };
        let old = make_flow();
        let current = make_flow();
        server.shared.streams.lock().insert(
            -1,
            Entry {
                flow: current.clone(),
            },
        );
        let pending = PendingOpen {
            shared: server.shared.clone(),
            flow: old.clone(),
            registered: true,
            completed: false,
        };
        drop(pending);
        assert!(Arc::ptr_eq(
            &server.shared.streams.lock().get(&-1).unwrap().flow,
            &current
        ));
        assert!(!server.shared.remove_flow(&old));
        assert!(server.shared.remove_flow(&current));
        client.close();
        server.close();
    }

    #[tokio::test]
    async fn keep_open_receive_timeout_does_not_cancel_whole_stream_deadline() {
        let (c, s) = tokio::io::duplex(1024);
        let (cr, cw) = tokio::io::split(c);
        let (sr, sw) = tokio::io::split(s);
        let client = Connection::start(
            cr,
            cw,
            StreamConfig {
                deadline: Some(std::time::Duration::from_millis(30)),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
        let server =
            Connection::start(sr, sw, StreamConfig::default(), None, Some(echo_router())).unwrap();
        let (_tx, mut rx) = client
            .open("Echo", "chat", Metadata::default())
            .await
            .unwrap()
            .into_bidi::<ApplicationException, ApplicationException>()
            .into_split();
        assert!(matches!(
            rx.recv_with_timeout_policy(
                std::time::Duration::from_millis(1),
                crate::streaming::RecvTimeoutPolicy::KeepOpen
            )
            .await,
            Err(StreamError::RecvTimeout)
        ));
        assert!(rx.context().cancellation().is_none());
        let error = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kitex_code(), Some(12015));
        client.close();
        server.close();
    }

    #[tokio::test]
    async fn closed_receive_queue_is_not_reported_as_capacity_overflow() {
        let (client, server) = pair(echo_router());
        let cx = StreamContext::new(42, "Echo", "chat", Role::Server);
        let (flow, _) = Flow::new(
            cx,
            server.shared.config.clone(),
            None,
            false,
            server.shared.recv_bytes.clone(),
            server.shared.send_bytes.clone(),
        );
        server
            .shared
            .streams
            .lock()
            .insert(42, Entry { flow: flow.clone() });
        // Handler/receiver may stop before its response Trailer is flushed.
        // A DATA already admitted by the reader must then be discarded, not RST.
        flow.inbound.close(true);
        let payload = Bytes::from_static(b"one message");
        let budget = ReceiveReservation::new(flow.clone(), payload.len()).unwrap();
        dispatch(
            Frame::new(42, FrameType::Data, Bytes::from_static(b"chat"), payload),
            Some(budget),
            &server.shared,
            None,
        )
        .unwrap();
        assert!(flow.cx.status().is_none());
        assert!(server.shared.streams.lock().contains_key(&42));
        assert_eq!(flow.cx.resources().queued_bytes, 0);
        client.close();
        server.close();
    }

    #[tokio::test]
    async fn open_does_not_wait_for_header_and_half_close_keeps_responses() {
        let (client, server) = pair(echo_router());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut stream = client
                .open("Echo", "chat", Metadata::default())
                .await
                .unwrap();
            stream.send_raw(Bytes::from_static(b"first")).await.unwrap();
            stream
                .send_raw(Bytes::from_static(b"second"))
                .await
                .unwrap();
            stream.close_send().await.unwrap();
            assert_eq!(
                stream.recv_raw().await.unwrap(),
                Some(Bytes::from_static(b"first"))
            );
            assert_eq!(
                stream.recv_raw().await.unwrap(),
                Some(Bytes::from_static(b"second"))
            );
            assert!(stream.recv_raw().await.unwrap().is_none());
            assert!(stream.context().status().unwrap().error.is_none());
        })
        .await
        .unwrap();
        client.close();
        server.close();
    }

    #[tokio::test]
    async fn unknown_route_finishes_and_other_stream_still_works() {
        let (client, server) = pair(echo_router());
        tokio::time::timeout(std::time::Duration::from_secs(2),async {
            let mut bad=client.open("Echo","absent",Metadata::default()).await.unwrap();
            assert!(matches!(bad.recv_raw().await,Err(StreamError::Application(e)) if e.kind()==ApplicationExceptionKind::UNKNOWN_METHOD));
            let mut good=client.open("unknown-isn","chat",Metadata::default()).await.unwrap();
            good.close_send().await.unwrap();assert!(good.recv_raw().await.unwrap().is_none());
        }).await.unwrap();
        client.close();
        server.close();
    }

    #[tokio::test]
    async fn rejection_resource_counters_distinguish_route_and_duplicate_decisions() {
        #[derive(Default)]
        struct Capture(Mutex<Vec<(i32, crate::streaming::StreamResources)>>);
        impl crate::streaming::StreamHook for Capture {
            fn on_finish(&self, cx: &StreamContext, status: &crate::streaming::StreamStatus) {
                self.0.lock().push((cx.stream_id(), status.resources));
            }
        }
        let capture = Arc::new(Capture::default());
        let hooks = crate::streaming::HookChain::new([
            capture.clone() as Arc<dyn crate::streaming::StreamHook>
        ]);
        let config = StreamConfig::default();
        let (mut wire, server_io) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(server_io);
        let server = Connection::start(
            reader,
            writer,
            config.clone(),
            Some(hooks),
            Some(echo_router()),
        )
        .unwrap();
        let mut header = Frame::new(
            41,
            FrameType::Header,
            Bytes::from_static(b"chat"),
            Bytes::new(),
        );
        header.metadata.insert(Bytes::from_static(b"isn"), "Echo");
        for _ in 0..2 {
            wire.write_all(&header.encode(&config).unwrap())
                .await
                .unwrap();
        }
        assert_eq!(
            read_frame(&mut wire, &config).await.unwrap().unwrap().kind,
            FrameType::Reset
        );
        wire.write_all(&header.encode(&config).unwrap())
            .await
            .unwrap();
        assert_eq!(
            read_frame(&mut wire, &config).await.unwrap().unwrap().kind,
            FrameType::Reset
        );
        header.stream_id = 42;
        header.method = Bytes::from_static(b"absent");
        wire.write_all(&header.encode(&config).unwrap())
            .await
            .unwrap();
        assert_eq!(
            read_frame(&mut wire, &config).await.unwrap().unwrap().kind,
            FrameType::Header
        );
        assert_eq!(
            read_frame(&mut wire, &config).await.unwrap().unwrap().kind,
            FrameType::Trailer
        );
        // Wrong service name still resolves a unique known method: not a route failure.
        header.stream_id = 43;
        header.method = Bytes::from_static(b"chat");
        header
            .metadata
            .insert(Bytes::from_static(b"isn"), "wrong-isn");
        wire.write_all(&header.encode(&config).unwrap())
            .await
            .unwrap();
        let trailer = Frame::new(43, FrameType::Trailer, header.method.clone(), Bytes::new());
        wire.write_all(&trailer.encode(&config).unwrap())
            .await
            .unwrap();
        assert_eq!(
            read_frame(&mut wire, &config).await.unwrap().unwrap().kind,
            FrameType::Header
        );
        assert_eq!(
            read_frame(&mut wire, &config).await.unwrap().unwrap().kind,
            FrameType::Trailer
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while capture.0.lock().len() < 4 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let observed = capture.0.lock().clone();
        assert_eq!(observed.len(), 4);
        for (sid, resources) in &observed {
            assert_eq!(resources.route_failures, u64::from(*sid == 42));
            assert_eq!(resources.duplicate_sid_rejections, u64::from(*sid == 41));
        }
        assert_eq!(observed.iter().filter(|(sid, _)| *sid == 41).count(), 2);
        server.close();
    }

    #[tokio::test]
    async fn drain_rejects_new_header_without_calling_handler() {
        let (client, server) = pair(echo_router());
        server.drain();
        let mut stream = client
            .open("Echo", "chat", Metadata::default())
            .await
            .unwrap();
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), stream.recv_raw())
            .await
            .unwrap()
            .unwrap_err();
        assert!(matches!(error, StreamError::Canceled { code: 6, .. }));
        client.close();
        server.close();
    }
    #[tokio::test]
    async fn reset_and_duplicate_header_never_emit_a_second_terminal() {
        for duplicate in [false, true] {
            let config = StreamConfig::default();
            let (mut wire, server_io) = tokio::io::duplex(4096);
            let (reader, writer) = tokio::io::split(server_io);
            let server =
                Connection::start(reader, writer, config.clone(), None, Some(echo_router()))
                    .unwrap();
            let mut header = Frame::new(
                10,
                FrameType::Header,
                Bytes::from_static(b"chat"),
                Bytes::new(),
            );
            header.metadata.insert(Bytes::from_static(b"isn"), "Echo");
            wire.write_all(&header.encode(&config).unwrap())
                .await
                .unwrap();
            let terminal = if duplicate {
                header
            } else {
                Frame::new(
                    10,
                    FrameType::Reset,
                    Bytes::from_static(b"chat"),
                    Bytes::new(),
                )
            };
            wire.write_all(&terminal.encode(&config).unwrap())
                .await
                .unwrap();
            let trailer = Frame::new(
                10,
                FrameType::Trailer,
                Bytes::from_static(b"chat"),
                Bytes::new(),
            );
            wire.write_all(&trailer.encode(&config).unwrap())
                .await
                .unwrap();
            if duplicate {
                let response = read_frame(&mut wire, &config).await.unwrap().unwrap();
                assert_eq!(response.kind, FrameType::Reset);
            }
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(50),
                    read_frame(&mut wire, &config)
                )
                .await
                .is_err()
            );
            server.close();
        }
    }

    #[tokio::test]
    async fn completed_short_lease_cannot_overwrite_success_with_connection_closed() {
        struct ShortLease(Connection);
        impl Drop for ShortLease {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let (client, server) = pair(echo_router());
        let mut stream = client
            .open("Echo", "chat", Metadata::default())
            .await
            .unwrap();
        stream.set_lease(Box::new(ShortLease(client.clone())));
        stream.send_raw(Bytes::from_static(b"ok")).await.unwrap();
        stream.close_send().await.unwrap();
        assert_eq!(
            stream.recv_raw().await.unwrap().unwrap(),
            Bytes::from_static(b"ok")
        );
        assert!(stream.recv_raw().await.unwrap().is_none());
        assert!(stream.context().status().unwrap().error.is_none());
        assert!(client.is_closed());
        server.close();
    }
    #[tokio::test]
    async fn outer_open_failure_still_sends_reset_for_its_already_opened_stream() {
        let config = StreamConfig::default();
        let (io, mut wire) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(io);
        let client = Connection::start(reader, writer, config.clone(), None, None).unwrap();
        let stream = client
            .open("Echo", "chat", Metadata::default())
            .await
            .unwrap();
        read_frame(&mut wire, &config).await.unwrap().unwrap();
        let cx = stream.context().clone();
        // A layer rejects after inner open; dropping its returned receiver
        // cancels locally, then OpenBoundary records the opening failure.
        drop(stream);
        assert!(cx.cancellation().is_some());
        assert!(cx.finish(crate::streaming::StreamStatus {
            error: Some(StreamError::configuration("outer layer rejected stream")),
            ..Default::default()
        }));
        let reset = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            read_frame(&mut wire, &config),
        )
        .await;
        client.close();
        assert_eq!(
            reset
                .expect("peer must receive RST after the outer open layer fails")
                .unwrap()
                .unwrap()
                .kind,
            FrameType::Reset
        );
    }

    #[tokio::test]
    async fn local_cancel_flushes_rst_before_releasing_short_lease() {
        struct ShortLease(Connection);
        impl Drop for ShortLease {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let config = StreamConfig::default();
        let (client_io, mut wire) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(client_io);
        let client = Connection::start(reader, writer, config.clone(), None, None).unwrap();
        let mut stream = client
            .open("Echo", "chat", Metadata::default())
            .await
            .unwrap();
        stream.set_lease(Box::new(ShortLease(client.clone())));
        assert_eq!(
            read_frame(&mut wire, &config).await.unwrap().unwrap().kind,
            FrameType::Header
        );
        stream.context().cancel(StreamError::Canceled {
            cause: CancelCause::Local,
            code: 12011,
            message: "cancel".into(),
            path: vec!["caller".into()],
        });
        let reset = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            read_frame(&mut wire, &config),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(reset.kind, FrameType::Reset);
        assert_eq!(reset.metadata.get("ttscp").unwrap(), "caller");
        client.closed().await;
    }

    #[tokio::test]
    async fn retiring_after_reset_waits_for_peer_terminal_or_bounded_timeout() {
        struct RetireLease(Connection);
        impl Drop for RetireLease {
            fn drop(&mut self) {
                self.0.retire();
            }
        }
        for peer_terminal in [true, false] {
            let config = StreamConfig {
                reset_retire_timeout: std::time::Duration::from_millis(100),
                ..Default::default()
            };
            let (io, mut wire) = tokio::io::duplex(4096);
            let (reader, writer) = tokio::io::split(io);
            let client = Connection::start(reader, writer, config.clone(), None, None).unwrap();
            let mut stream = client
                .open("Echo", "chat", Metadata::default())
                .await
                .unwrap();
            stream.set_lease(Box::new(RetireLease(client.clone())));
            let id = stream.context().stream_id();
            read_frame(&mut wire, &config).await.unwrap().unwrap();
            stream.context().cancel_local("retire test");
            let reset = read_frame(&mut wire, &config).await.unwrap().unwrap();
            assert_eq!(reset.kind, FrameType::Reset);
            assert_eq!(
                crate::streaming::decode_exception(reset.payload)
                    .unwrap()
                    .kind()
                    .as_i32(),
                12007
            );
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(10), client.closed())
                    .await
                    .is_err()
            );
            if peer_terminal {
                let trailer = Frame::new(
                    id,
                    FrameType::Trailer,
                    Bytes::from_static(b"chat"),
                    Bytes::new(),
                );
                wire.write_all(&trailer.encode(&config).unwrap())
                    .await
                    .unwrap();
            }
            tokio::time::timeout(std::time::Duration::from_millis(200), client.closed())
                .await
                .unwrap();
            client.retire();
            client.close();
            assert!(read_frame(&mut wire, &config).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn reset_releases_queued_payload_without_application_polling() {
        let config = StreamConfig::default();
        let (client_io, mut wire) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(client_io);
        let client = Connection::start(reader, writer, config.clone(), None, None).unwrap();
        let stream = client
            .open("Echo", "chat", Metadata::default())
            .await
            .unwrap();
        let id = stream.context().stream_id();
        read_frame(&mut wire, &config).await.unwrap();
        for _ in 0..2 {
            wire.write_all(
                &Frame::new(
                    id,
                    FrameType::Data,
                    Bytes::from_static(b"chat"),
                    Bytes::from_static(b"payload"),
                )
                .encode(&config)
                .unwrap(),
            )
            .await
            .unwrap();
        }
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while stream.context().resources().queued_messages != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        wire.write_all(
            &Frame::new(
                id,
                FrameType::Reset,
                Bytes::from_static(b"chat"),
                Bytes::new(),
            )
            .encode(&config)
            .unwrap(),
        )
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while stream.context().status().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(stream.context().resources().queued_bytes, 0);
        assert_eq!(stream.context().resources().queued_messages, 0);
        assert_eq!(stream.context().resources().high_water_messages, 2);
        assert_eq!(client.shared.recv_bytes.load(Ordering::Relaxed), 0);
        client.close();
    }

    #[tokio::test]
    async fn receive_overflow_and_bounded_pause_preserve_other_streams() {
        for pause in [
            std::time::Duration::ZERO,
            std::time::Duration::from_millis(25),
        ] {
            let captured = Arc::new(Mutex::new(None::<StreamContext>));
            let ready = Arc::new(Notify::new());
            let mut router = echo_router();
            router
                .add("Echo", "hold", {
                    let captured = captured.clone();
                    let ready = ready.clone();
                    move |cx, stream| {
                        *captured.lock() = Some(cx);
                        ready.notify_one();
                        async move {
                            let _stream = stream;
                            futures::future::pending::<Result<(), StreamError>>().await
                        }
                    }
                })
                .unwrap();
            let config = StreamConfig {
                recv_messages: 1,
                recv_bytes: 4,
                connection_recv_bytes: 16,
                max_read_pause: pause,
                ..Default::default()
            };
            let (mut wire, server_io) = tokio::io::duplex(4096);
            let (reader, writer) = tokio::io::split(server_io);
            let server =
                Connection::start(reader, writer, config.clone(), None, Some(router)).unwrap();
            for frame in [
                Frame::new(
                    1,
                    FrameType::Header,
                    Bytes::from_static(b"hold"),
                    Bytes::new(),
                ),
                Frame::new(
                    1,
                    FrameType::Data,
                    Bytes::from_static(b"hold"),
                    Bytes::from_static(b"full"),
                ),
                Frame::new(
                    1,
                    FrameType::Data,
                    Bytes::from_static(b"hold"),
                    Bytes::from(vec![b'x'; 32768]),
                ),
            ] {
                wire.write_all(&frame.encode(&config).unwrap())
                    .await
                    .unwrap();
                if frame.kind == FrameType::Header {
                    tokio::time::timeout(std::time::Duration::from_secs(1), ready.notified())
                        .await
                        .unwrap();
                }
            }
            let reset = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                read_frame(&mut wire, &config),
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap();
            assert_eq!(reset.kind, FrameType::Reset);
            let cx = captured.lock().clone().unwrap();
            assert!(matches!(
                cx.status().unwrap().error,
                Some(StreamError::ResourceExhausted { .. })
            ));
            assert_eq!(cx.resources().queued_bytes, 0);
            assert_eq!(cx.resources().buffer_limit_resets, 1);
            if !pause.is_zero() {
                assert!(cx.resources().read_pauses > 0);
            }
            wire.write_all(
                &Frame::new(
                    2,
                    FrameType::Header,
                    Bytes::from_static(b"chat"),
                    Bytes::new(),
                )
                .encode(&config)
                .unwrap(),
            )
            .await
            .unwrap();
            wire.write_all(
                &Frame::new(
                    2,
                    FrameType::Trailer,
                    Bytes::from_static(b"chat"),
                    Bytes::new(),
                )
                .encode(&config)
                .unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(
                read_frame(&mut wire, &config).await.unwrap().unwrap().kind,
                FrameType::Header
            );
            assert_eq!(
                read_frame(&mut wire, &config).await.unwrap().unwrap().kind,
                FrameType::Trailer
            );
            server.close();
        }
    }

    #[tokio::test]
    async fn graceful_deadline_sends_rst_and_releases_server_driver() {
        let config = StreamConfig {
            drain_timeout: std::time::Duration::from_millis(20),
            shutdown_flush_timeout: std::time::Duration::from_millis(100),
            ..Default::default()
        };
        let (client_io, server_io) = tokio::io::duplex(4096);
        let (cr, cw) = tokio::io::split(client_io);
        let (sr, sw) = tokio::io::split(server_io);
        let client = Connection::start(cr, cw, config.clone(), None, None).unwrap();
        let (shutdown, stop) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(Connection::serve(
            sr,
            sw,
            config,
            None,
            echo_router(),
            async {
                let _ = stop.await;
            },
        ));
        let mut stream = client
            .open("Echo", "chat", Metadata::default())
            .await
            .unwrap();
        stream
            .send_raw(Bytes::from_static(b"started"))
            .await
            .unwrap();
        assert!(stream.recv_raw().await.unwrap().is_some());
        shutdown.send(()).unwrap();
        let error = tokio::time::timeout(std::time::Duration::from_secs(1), stream.recv_raw())
            .await
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(error,StreamError::Canceled {code:6,ref message,..} if message.contains("server draining"))
        );
        server.await.unwrap().unwrap();
        client.close();
    }
    #[tokio::test]
    async fn handler_scope_observes_authoritative_context_and_maps_final_error() {
        use crate::streaming::{
            STREAM_CONTEXT, StreamHandlerFuture, StreamHandlerScope, StreamMode,
        };
        struct Scope;
        impl StreamHandlerScope for Scope {
            fn scope(&self, cx: StreamContext, future: StreamHandlerFuture) -> StreamHandlerFuture {
                assert_eq!(
                    STREAM_CONTEXT.with(|current| current.stream_id()),
                    cx.stream_id()
                );
                Box::pin(async move {
                    use metainfo::Forward;
                    metainfo::METAINFO
                        .with(|info| info.borrow_mut().set_transient("scope", "present"));
                    future.await.map_err(|_| StreamError::Biz {
                        code: 321,
                        message: "mapped".into(),
                        extra: Default::default(),
                    })
                })
            }
        }
        let mut router = StreamRouter::new();
        router.handler_scope(Arc::new(Scope));
        router
            .add_mode(
                "Echo",
                "chat",
                StreamMode::Bidirectional,
                |cx, _stream| async move {
                    use metainfo::Forward;
                    assert_eq!(cx.mode(), StreamMode::Bidirectional);
                    assert_eq!(
                        cx.with_metainfo(|info| info.get_transient("scope"))
                            .as_deref(),
                        Some("present")
                    );
                    Err(StreamError::Protocol("business".into()))
                },
            )
            .unwrap();
        let (client, server) = pair(router);
        let mut stream = client
            .open("Echo", "chat", Metadata::default())
            .await
            .unwrap();
        assert!(matches!(
            stream.recv_raw().await,
            Err(StreamError::Biz { code: 321, .. })
        ));
        client.close();
        server.close();
    }
    #[tokio::test]
    async fn server_local_deadline_is_scheduled_when_header_arrives() {
        let config = StreamConfig {
            deadline: Some(std::time::Duration::from_millis(10)),
            ..Default::default()
        };
        let mut router = StreamRouter::new();
        router
            .add("Echo", "hold", |_, stream| async move {
                let _stream = stream;
                std::future::pending().await
            })
            .unwrap();
        let (mut wire, io) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(io);
        let server = Connection::start(reader, writer, config.clone(), None, Some(router)).unwrap();
        let header = Frame::new(
            1,
            FrameType::Header,
            Bytes::from_static(b"hold"),
            Bytes::new(),
        );
        wire.write_all(&header.encode(&config).unwrap())
            .await
            .unwrap();
        let frame = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            read_frame(&mut wire, &config),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(frame.kind, FrameType::Reset);
        assert_eq!(
            crate::streaming::decode_exception(frame.payload)
                .unwrap()
                .kind()
                .as_i32(),
            12015
        );
        server.close();
    }
    #[tokio::test]
    async fn idle_connection_has_no_periodic_read_polling() {
        struct PendingReader(Arc<AtomicUsize>);
        impl AsyncRead for PendingReader {
            fn poll_read(
                self: Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                _: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                self.0.fetch_add(1, Ordering::Relaxed);
                std::task::Poll::Pending
            }
        }
        let polls = Arc::new(AtomicUsize::new(0));
        let config = StreamConfig {
            pool_idle_timeout: std::time::Duration::from_millis(80),
            ..Default::default()
        };
        let connection = Connection::start(
            PendingReader(polls.clone()),
            tokio::io::sink(),
            config,
            None,
            None,
        )
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), connection.closed())
            .await
            .unwrap();
        assert!(
            polls.load(Ordering::Relaxed) <= 2,
            "idle socket should only be polled initially, not every10ms"
        );
    }
    #[tokio::test]
    async fn cancel_between_partial_writes_finishes_started_frame_and_discards_later_data() {
        use std::task::{Context, Poll};
        struct State {
            stage: AtomicUsize,
            bytes: Mutex<Vec<u8>>,
            written: Notify,
            flushed: Notify,
            waker: futures::task::AtomicWaker,
        }
        struct Writer(Arc<State>);
        impl AsyncWrite for Writer {
            fn poll_write(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                bytes: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                match self.0.stage.load(Ordering::Acquire) {
                    0 => {
                        let count = bytes.len().min(3);
                        self.0.bytes.lock().extend_from_slice(&bytes[..count]);
                        self.0.stage.store(1, Ordering::Release);
                        self.0.written.notify_one();
                        Poll::Ready(Ok(count))
                    }
                    1 => {
                        self.0.waker.register(cx.waker());
                        Poll::Pending
                    }
                    _ => {
                        self.0.bytes.lock().extend_from_slice(bytes);
                        Poll::Ready(Ok(bytes.len()))
                    }
                }
            }
            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                self.0.flushed.notify_one();
                Poll::Ready(Ok(()))
            }
            fn poll_shutdown(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }
        let config = Arc::new(StreamConfig::default());
        let (tx, rx) = mpsc::channel(4);
        let (control, controls) = mpsc::channel(4);
        let shared = Arc::new(Shared {
            streams: Mutex::default(),
            handlers: None,
            quarantine: Mutex::default(),
            recv_bytes: Arc::default(),
            send_bytes: Arc::default(),
            closed: AtomicBool::new(false),
            retiring: AtomicBool::new(false),
            reset_grace: Mutex::new(None),
            draining: AtomicBool::new(false),
            wake: Arc::new(Notify::new()),
            read_wake: Arc::new(Notify::new()),
            tx: tx.clone(),
            control,
            config: config.clone(),
            hooks: None,
            idle_since: Mutex::new(None),
        });
        let cx = StreamContext::new(1, "S", "M", Role::Client);
        let (flow, _) = Flow::new(
            cx,
            config.clone(),
            None,
            true,
            shared.recv_bytes.clone(),
            shared.send_bytes.clone(),
        );
        let first = Frame::new(
            1,
            FrameType::Data,
            Bytes::from_static(b"M"),
            Bytes::from_static(b"first"),
        )
        .encode(&config)
        .unwrap();
        let second = Frame::new(
            1,
            FrameType::Data,
            Bytes::from_static(b"M"),
            Bytes::from_static(b"second"),
        )
        .encode(&config)
        .unwrap();
        for bytes in [first.clone(), second] {
            tx.send(WriteCommand {
                bytes,
                flow: Some(flow.clone()),
                finish: None,
                flushed: None,
                control_observer: None,
                reset_id: None,
                _budget: None,
                _lease: None,
            })
            .await
            .unwrap();
        }
        let state = Arc::new(State {
            stage: AtomicUsize::new(0),
            bytes: Mutex::default(),
            written: Notify::new(),
            flushed: Notify::new(),
            waker: Default::default(),
        });
        let writer = tokio::spawn(write_loop(
            Writer(state.clone()),
            rx,
            controls,
            shared.clone(),
        ));
        state.written.notified().await;
        flow.cx.cancel_local("cancel during partial write");
        flow.finish(flow.cx.cancellation());
        shared.wake.notify_waiters();
        state.stage.store(2, Ordering::Release);
        state.waker.wake();
        tokio::time::timeout(std::time::Duration::from_secs(1), state.flushed.notified())
            .await
            .unwrap();
        assert_eq!(&*state.bytes.lock(), first.as_ref());
        close(&shared, StreamError::closed());
        writer.await.unwrap();
    }
    #[tokio::test]
    async fn raw_headers_are_delayed_and_trailers_cover_both_receive_roles() {
        let ready = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut router = StreamRouter::new();
        router
            .add("S", "M", {
                let ready = ready.clone();
                let release = release.clone();
                move |_, stream| {
                    let ready = ready.clone();
                    let release = release.clone();
                    async move {
                        let (mut tx, mut rx) = stream.into_raw_split();
                        assert_eq!(rx.header().await?.get("request").unwrap(), "header");
                        assert!(rx.trailer().is_none());
                        let mut header = Metadata::default();
                        header.insert(
                            Bytes::from_static(b"response"),
                            Bytes::from_static(b"cached"),
                        );
                        tx.set_header(header)?;
                        ready.notify_one();
                        release.notified().await;
                        assert!(rx.recv().await?.is_none());
                        assert_eq!(rx.trailer().unwrap().get("request").unwrap(), "trailer");
                        let mut trailer = Metadata::default();
                        trailer
                            .insert(Bytes::from_static(b"response"), Bytes::from_static(b"done"));
                        tx.set_trailer(trailer)?;
                        tx.send(Bytes::from_static(b"data")).await?;
                        Ok(())
                    }
                }
            })
            .unwrap();
        let (client, server) = pair(router);
        let mut metadata = Metadata::default();
        metadata.insert(
            Bytes::from_static(b"request"),
            Bytes::from_static(b"header"),
        );
        let (mut tx, mut rx) = client
            .open("S", "M", metadata)
            .await
            .unwrap()
            .into_raw_split();
        ready.notified().await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(5), rx.header())
                .await
                .is_err()
        );
        let mut trailer = Metadata::default();
        trailer.insert(
            Bytes::from_static(b"request"),
            Bytes::from_static(b"trailer"),
        );
        tx.set_trailer(trailer).unwrap();
        tx.close_send().await.unwrap();
        release.notify_one();
        assert_eq!(
            rx.header().await.unwrap().get("response").unwrap(),
            "cached"
        );
        assert_eq!(rx.recv().await.unwrap().unwrap(), "data");
        assert!(rx.recv().await.unwrap().is_none());
        assert_eq!(rx.trailer().unwrap().get("response").unwrap(), "done");
        client.close();
        server.close();
    }
    #[tokio::test]
    async fn duplicate_response_header_closes_connection_with_kitex_12002() {
        let config = StreamConfig::default();
        let (io, mut wire) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(io);
        let client = Connection::start(reader, writer, config.clone(), None, None).unwrap();
        let mut stream = client.open("S", "M", Metadata::default()).await.unwrap();
        let sid = stream.context().stream_id();
        read_frame(&mut wire, &config).await.unwrap();
        let header = Frame::new(
            sid,
            FrameType::Header,
            Bytes::from_static(b"M"),
            Bytes::new(),
        )
        .encode(&config)
        .unwrap();
        wire.write_all(&header).await.unwrap();
        wire.write_all(&header).await.unwrap();
        let error = tokio::time::timeout(std::time::Duration::from_secs(1), stream.recv_raw())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kitex_code(), Some(12002));
        assert!(client.is_closed());
    }
}

#[cfg(all(test, feature = "ttstream"))]
#[path = "connection_acceptance_tests.rs"]
mod acceptance_audit;

#[cfg(all(test, feature = "ttstream"))]
#[path = "handler_lifetime_tests.rs"]
mod handler_lifetime_tests;
