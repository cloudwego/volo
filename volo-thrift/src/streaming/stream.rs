use std::{
    collections::VecDeque,
    marker::PhantomData,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::Stream;
use parking_lot::Mutex;
use pilota::thrift::{Message, TLengthProtocol};
use tokio::sync::{Notify, mpsc};

use super::{
    CancelCause, HookChain, Metadata, StreamConfig, StreamContext, StreamError, StreamHook,
    StreamStatus, decode_message,
};
use crate::transport::ttstream::frame::{Frame, FrameBytes, FrameTemplate, FrameType};

pub(crate) struct Flow {
    pub cx: StreamContext,
    pub config: Arc<StreamConfig>,
    pub hooks: Option<HookChain>,
    pub inbound: Arc<ReceiveQueue>,
    pub header: Mutex<Option<Metadata>>,
    pub trailer: Mutex<Metadata>,
    pub recv_error: Mutex<Option<StreamError>>,
    pub header_notify: Notify,
    pub discard: AtomicBool,
    pub recv_closed: AtomicBool,
    pub send_closed: AtomicBool,
    pub header_sent: AtomicBool,
    pub recv_bytes: AtomicUsize,
    pub connection_recv_bytes: Arc<AtomicUsize>,
    pub send_bytes: AtomicUsize,
    pub connection_send_bytes: Arc<AtomicUsize>,
    pub sent_count: AtomicU64,
    pub sent_total: AtomicU64,
    pub recv_count: AtomicU64,
    pub recv_total: AtomicU64,
    pub client: bool,
    pub lease: Mutex<Option<Box<dyn Send + Sync>>>,
    receive_done: AtomicBool,
    observation_done: AtomicBool,
    owns_terminal: AtomicBool,
    wire_finished: AtomicBool,
    pending_observation: AtomicUsize,
    observation_error: Mutex<Option<StreamError>>,
}

const OBSERVATION_CLOSED: usize = 1 << (usize::BITS - 1);

impl Flow {
    pub fn new(
        cx: StreamContext,
        config: Arc<StreamConfig>,
        hooks: Option<HookChain>,
        client: bool,
        connection_recv_bytes: Arc<AtomicUsize>,
        connection_send_bytes: Arc<AtomicUsize>,
    ) -> (Arc<Self>, Arc<ReceiveQueue>) {
        let hooks = cx
            .inner
            .hooks
            .get()
            .cloned()
            .or_else(|| hooks.map(|hooks| hooks.for_call()));
        if let Some(hooks) = &hooks {
            let _ = cx.inner.hooks.set(hooks.clone());
        }
        let queue = Arc::new(ReceiveQueue::new(config.recv_messages));
        cx.initialize_outgoing(client);
        (
            Arc::new(Self {
                cx,
                config,
                hooks,
                inbound: queue.clone(),
                header: Mutex::new(None),
                trailer: Mutex::default(),
                recv_error: Mutex::new(None),
                header_notify: Notify::new(),
                discard: AtomicBool::new(false),
                recv_closed: AtomicBool::new(false),
                send_closed: AtomicBool::new(false),
                header_sent: AtomicBool::new(client),
                recv_bytes: AtomicUsize::new(0),
                connection_recv_bytes,
                connection_send_bytes,
                send_bytes: AtomicUsize::new(0),
                sent_count: AtomicU64::new(0),
                sent_total: AtomicU64::new(0),
                recv_count: AtomicU64::new(0),
                recv_total: AtomicU64::new(0),
                client,
                lease: Mutex::new(None),
                receive_done: AtomicBool::new(true),
                observation_done: AtomicBool::new(false),
                owns_terminal: AtomicBool::new(false),
                wire_finished: AtomicBool::new(false),
                pending_observation: AtomicUsize::new(0),
                observation_error: Mutex::new(None),
            }),
            queue,
        )
    }
    pub(crate) fn wire_finished(&self) -> bool {
        self.wire_finished.load(Ordering::Acquire)
    }
    pub(crate) fn observe(&self) -> Option<FlowObservation<'_>> {
        self.hooks.as_ref()?;
        let mut state = self.pending_observation.load(Ordering::Acquire);
        loop {
            if state & OBSERVATION_CLOSED != 0 {
                return None;
            }
            match self.pending_observation.compare_exchange_weak(
                state,
                state + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(FlowObservation(self)),
                Err(value) => state = value,
            }
        }
    }
    pub(crate) fn with_hooks(&self, callback: impl FnOnce(&HookChain)) {
        if let Some(hooks) = &self.hooks {
            if let Some(_observation) = self.observe() {
                callback(hooks);
            }
        }
    }
    pub fn finish(&self, error: Option<StreamError>) {
        // Mark before publishing context status: maintenance must distinguish
        // an external opening-layer failure from a terminal this flow owns.
        self.wire_finished.store(true, Ordering::Release);
        if self.cx.cancellation().is_some() {
            self.inbound.close(true);
        }
        let status = StreamStatus {
            resources: self.cx.resources(),
            error,
            header: self.header.lock().clone().unwrap_or_default(),
            trailer: self.trailer.lock().clone(),
            sent_messages: self.sent_count.load(Ordering::Relaxed),
            sent_bytes: self.sent_total.load(Ordering::Relaxed),
            received_messages: self.recv_count.load(Ordering::Relaxed),
            received_bytes: self.recv_total.load(Ordering::Relaxed),
        };
        if self.cx.finish(status.clone()) {
            self.owns_terminal.store(true, Ordering::Release);
            let lease = self.lease.lock().take();
            drop(lease);
            self.inbound.close(self.cx.cancellation().is_some());
            self.header_notify.notify_waiters();
        }
        self.publish_observation();
    }
    // Protocol termination releases leases and cancels children immediately. A
    // client can still own decoded DATA queued before Trailer, so its final hook
    // waits for application EOF/error or receiver drop instead.
    fn publish_observation(&self) {
        // An outer opening layer can fail after constructing an inner stream.
        // OpenBoundary owns that terminal; receiver drop must not publish it twice.
        if !self.owns_terminal.load(Ordering::Acquire)
            || self.pending_observation.load(Ordering::Acquire) != 0
        {
            return;
        }
        if self.client && !self.receive_done.load(Ordering::Acquire) {
            return;
        }
        let Some(mut status) = self.cx.status() else {
            return;
        };
        if self
            .pending_observation
            .compare_exchange(0, OBSERVATION_CLOSED, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        self.observation_done.store(true, Ordering::Release);
        status.resources = self.cx.resources();
        status.sent_messages = self.sent_count.load(Ordering::Relaxed);
        status.sent_bytes = self.sent_total.load(Ordering::Relaxed);
        status.received_messages = self.recv_count.load(Ordering::Relaxed);
        status.received_bytes = self.recv_total.load(Ordering::Relaxed);
        if let Some(error) = self.observation_error.lock().clone() {
            status.error = Some(error);
        }
        if let Some(hooks) = &self.hooks {
            hooks.on_finish(&self.cx, &status);
        }
    }
    fn finish_receive(&self) {
        self.receive_done.store(true, Ordering::Release);
        self.publish_observation();
    }
    pub(crate) fn io_error(
        &self,
        direction: super::IoDirection,
        phase: super::IoPhase,
        size: Option<usize>,
        error: StreamError,
    ) {
        self.with_hooks(|hooks| {
            hooks.on_io_error(
                &self.cx,
                &super::StreamIoError {
                    at: std::time::Instant::now(),
                    direction,
                    phase,
                    size,
                    error,
                },
            );
        });
    }
    fn send_error(&self, error: &StreamError, size: Option<usize>) {
        if matches!(error, StreamError::ResourceExhausted { .. }) {
            self.io_error(
                super::IoDirection::Send,
                super::IoPhase::Encode,
                size,
                error.clone(),
            );
        } else {
            self.codec_error(
                super::IoDirection::Send,
                super::IoPhase::Encode,
                size,
                error.clone(),
            );
        }
    }
    fn codec_error(
        &self,
        direction: super::IoDirection,
        phase: super::IoPhase,
        size: Option<usize>,
        error: StreamError,
    ) {
        self.io_error(direction, phase, size, error.clone());
        if self.client && matches!(direction, super::IoDirection::Recv) {
            *self.observation_error.lock() = Some(error.clone());
        }
        if self.cx.status().is_none() {
            self.cx.cancel(error);
        }
    }
    pub fn frame(&self, kind: FrameType, payload: Bytes) -> Frame {
        Frame::new(
            self.cx.stream_id(),
            kind,
            self.cx.inner.method.clone().into_bytes(),
            payload,
        )
    }
}

/// A single-reader queue whose producer can discard buffered packets on reset.
/// Keeping cancellation separate from normal EOF preserves DATA-before-Trailer order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReceivePushError {
    Closed,
    Full,
}

pub(crate) struct ReceiveQueue {
    state: Mutex<ReceiveState>,
    capacity: usize,
    waker: futures::task::AtomicWaker,
}
#[derive(Default)]
struct ReceiveState {
    packets: VecDeque<Packet>,
    closed: bool,
}
impl ReceiveQueue {
    fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::default(),
            capacity,
            waker: futures::task::AtomicWaker::new(),
        }
    }
    pub fn is_full(&self) -> bool {
        self.state.lock().packets.len() >= self.capacity
    }
    pub fn push(&self, packet: Packet) -> Result<(), ReceivePushError> {
        let mut state = self.state.lock();
        if state.closed {
            return Err(ReceivePushError::Closed);
        }
        if state.packets.len() >= self.capacity {
            return Err(ReceivePushError::Full);
        }
        state.packets.push_back(packet);
        drop(state);
        self.waker.wake();
        Ok(())
    }
    pub fn close(&self, discard: bool) {
        let discarded = {
            let mut state = self.state.lock();
            state.closed = true;
            discard.then(|| std::mem::take(&mut state.packets))
        };
        drop(discarded);
        self.waker.wake();
    }
    fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Option<Packet>> {
        self.waker.register(cx.waker());
        let mut state = self.state.lock();
        if let Some(packet) = state.packets.pop_front() {
            return Poll::Ready(Some(packet));
        }
        if state.closed {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}

pub(crate) struct Packet {
    pub bytes: Bytes,
    pub _budget: ReceiveReservation,
}

pub(crate) struct ReceiveReservation {
    pub(crate) flow: Arc<Flow>,
    len: usize,
}
impl ReceiveReservation {
    pub fn new(flow: Arc<Flow>, len: usize) -> Result<Self, StreamError> {
        let counters = [
            (&flow.recv_bytes, flow.config.recv_bytes, "stream bytes"),
            (
                &*flow.connection_recv_bytes,
                flow.config.connection_recv_bytes,
                "connection bytes",
            ),
            (
                &SHARED_RECV_BYTES,
                flow.config.shared_recv_bytes,
                "shared bytes",
            ),
        ];
        for (index, (counter, limit, scope)) in counters.iter().enumerate() {
            if let Err(observed) = reserve_bytes(counter, len, *limit) {
                for (counter, _, _) in &counters[..index] {
                    counter.fetch_sub(len, Ordering::Relaxed);
                }
                return Err(StreamError::ResourceExhausted {
                    scope,
                    limit: *limit,
                    observed: observed.saturating_add(len),
                });
            }
        }
        let resources = &flow.cx.inner.resources;
        let messages = resources.queued_messages.fetch_add(1, Ordering::Relaxed) + 1;
        let bytes = resources
            .queued_bytes
            .fetch_add(len as u64, Ordering::Relaxed)
            + len as u64;
        resources
            .high_water_messages
            .fetch_max(messages, Ordering::Relaxed);
        resources
            .high_water_bytes
            .fetch_max(bytes, Ordering::Relaxed);
        Ok(Self { flow, len })
    }
}
impl Drop for ReceiveReservation {
    fn drop(&mut self) {
        self.flow
            .cx
            .inner
            .resources
            .queued_messages
            .fetch_sub(1, Ordering::Relaxed);
        self.flow
            .cx
            .inner
            .resources
            .queued_bytes
            .fetch_sub(self.len as u64, Ordering::Relaxed);
        self.flow.recv_bytes.fetch_sub(self.len, Ordering::Relaxed);
        self.flow
            .connection_recv_bytes
            .fetch_sub(self.len, Ordering::Relaxed);
        SHARED_RECV_BYTES.fetch_sub(self.len, Ordering::Relaxed);
        if !self.flow.config.max_read_pause.is_zero() {
            if let Some(wake) = self.flow.cx.inner.driver_notify.lock().as_ref() {
                wake.notify_one();
            }
        }
    }
}

pub(crate) struct FlowObservation<'a>(&'a Flow);
impl Drop for FlowObservation<'_> {
    fn drop(&mut self) {
        if self.0.pending_observation.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.publish_observation();
        }
    }
}

pub(crate) struct ControlObservation {
    queued: std::time::Instant,
    target: ControlTarget,
    flushed: bool,
}
enum ControlTarget {
    Flow(Arc<Flow>),
    Rejected { cx: StreamContext, hooks: HookChain },
}
impl ControlObservation {
    pub(crate) fn flow(flow: Arc<Flow>) -> Self {
        flow.pending_observation.fetch_add(1, Ordering::AcqRel);
        Self {
            queued: std::time::Instant::now(),
            target: ControlTarget::Flow(flow),
            flushed: false,
        }
    }
    pub(crate) fn rejected(cx: StreamContext, hooks: HookChain) -> Self {
        Self {
            queued: std::time::Instant::now(),
            target: ControlTarget::Rejected { cx, hooks },
            flushed: false,
        }
    }
    pub(crate) fn flushed(&mut self) {
        self.flushed = true;
        let cx = match &self.target {
            ControlTarget::Flow(flow) => &flow.cx,
            ControlTarget::Rejected { cx, .. } => cx,
        };
        cx.inner.resources.control_queue_delay_nanos.store(
            super::resources::nanos(self.queued.elapsed()),
            Ordering::Relaxed,
        );
    }
}
impl Drop for ControlObservation {
    fn drop(&mut self) {
        // Write/encode/channel failure drops the same guard. Final observation
        // must converge even when the control frame cannot be flushed.
        let (cx, hooks) = match &self.target {
            ControlTarget::Flow(flow) => (&flow.cx, flow.hooks.as_ref()),
            ControlTarget::Rejected { cx, hooks } => (cx, Some(hooks)),
        };
        if !self.flushed {
            let error = StreamError::transport(
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "control frame discarded before flush",
                ),
                super::IoPhase::Flush,
                true,
            );
            if let Some(hooks) = hooks {
                hooks.on_io_error(
                    cx,
                    &super::StreamIoError {
                        at: std::time::Instant::now(),
                        direction: super::IoDirection::Send,
                        phase: super::IoPhase::Flush,
                        size: None,
                        error,
                    },
                );
            }
        }
        match &self.target {
            ControlTarget::Flow(flow) => {
                if flow.pending_observation.fetch_sub(1, Ordering::AcqRel) == 1 {
                    flow.publish_observation();
                }
            }
            ControlTarget::Rejected { cx, hooks } => {
                if let Some(mut status) = cx.status() {
                    status.resources = cx.resources();
                    hooks.on_finish(cx, &status);
                }
            }
        }
    }
}

pub(crate) struct WriteCommand {
    pub bytes: FrameBytes,
    pub flow: Option<Arc<Flow>>,
    pub finish: Option<Option<StreamError>>,
    pub flushed: Option<tokio::sync::oneshot::Sender<()>>,
    pub control_observer: Option<ControlObservation>,
    pub reset_id: Option<i32>,
    pub _budget: Option<SendBudget>,
    pub _lease: Option<Box<dyn Send + Sync>>,
}

pub(crate) struct SendCore {
    pub flow: Arc<Flow>,
    pub tx: mpsc::Sender<WriteCommand>,
    pub template: FrameTemplate,
}

pub(crate) struct SendBudget {
    flow: Arc<Flow>,
    bytes: usize,
}
impl SendBudget {
    fn reserve(flow: Arc<Flow>, bytes: usize, scope: &'static str) -> Result<Self, StreamError> {
        let limits = [
            (&flow.send_bytes, flow.config.send_staging_bytes, scope),
            (
                &*flow.connection_send_bytes,
                flow.config.connection_send_bytes,
                "connection send bytes",
            ),
            (
                &SHARED_SEND_BYTES,
                flow.config.shared_send_bytes,
                "shared send bytes",
            ),
        ];
        for (index, (counter, limit, scope)) in limits.iter().enumerate() {
            if let Err(old) = reserve_bytes(counter, bytes, *limit) {
                for (previous, _, _) in &limits[..index] {
                    previous.fetch_sub(bytes, Ordering::Relaxed);
                }
                return Err(StreamError::ResourceExhausted {
                    scope,
                    limit: *limit,
                    observed: old.saturating_add(bytes),
                });
            }
        }
        let total = flow
            .cx
            .inner
            .resources
            .send_buffer_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed)
            + bytes as u64;
        flow.cx
            .inner
            .resources
            .send_high_water_bytes
            .fetch_max(total, Ordering::Relaxed);
        Ok(Self { flow, bytes })
    }
}
impl Drop for SendBudget {
    fn drop(&mut self) {
        self.flow
            .send_bytes
            .fetch_sub(self.bytes, Ordering::Relaxed);
        self.flow
            .connection_send_bytes
            .fetch_sub(self.bytes, Ordering::Relaxed);
        SHARED_SEND_BYTES.fetch_sub(self.bytes, Ordering::Relaxed);
        self.flow
            .cx
            .inner
            .resources
            .send_buffer_bytes
            .fetch_sub(self.bytes as u64, Ordering::Relaxed);
    }
}
struct Staged {
    bytes: FrameBytes,
    payload_len: usize,
    _budget: SendBudget,
}

impl SendCore {
    fn stage<T: Message>(&self, value: &T) -> Result<Staged, StreamError> {
        self.check_send()?;
        let mut sizing = pilota::thrift::binary::TBinaryProtocol::new((), true);
        let size = value.size(&mut sizing);
        let zero_copy_len = sizing.zero_copy_len();
        self.stage_with(size, || {
            if zero_copy_len == 0 {
                // Keep small messages on the single-allocation path. They do
                // not need LinkedBytes' node and I/O-slice collections.
                self.template
                    .encode_with(
                        self.flow.cx.stream_id(),
                        size,
                        &self.flow.config,
                        |output| {
                            super::codec::encode_into(value, output, self.flow.config.unsafe_codec)
                        },
                    )
                    .map(Into::into)
            } else {
                self.template.encode_linked(
                    self.flow.cx.stream_id(),
                    size,
                    zero_copy_len,
                    &self.flow.config,
                    |output| {
                        super::codec::encode_linked_into(
                            value,
                            output,
                            self.flow.config.unsafe_codec,
                        )
                    },
                )
            }
        })
    }
    fn stage_encoded(
        &self,
        size: usize,
        encode: impl FnOnce() -> Result<Bytes, StreamError>,
    ) -> Result<Staged, StreamError> {
        self.stage_with(size, || {
            let payload = encode()?;
            if payload.len() != size {
                return Err(StreamError::Protocol(
                    "encoded size disagrees with reserved length".into(),
                ));
            }
            self.template
                .encode_bytes(self.flow.cx.stream_id(), payload, &self.flow.config)
        })
    }
    fn stage_with(
        &self,
        size: usize,
        encode: impl FnOnce() -> Result<FrameBytes, StreamError>,
    ) -> Result<Staged, StreamError> {
        self.check_send()?;
        let frame_limit = self.template.payload_limit(&self.flow.config);
        if size > frame_limit {
            return Err(StreamError::ResourceExhausted {
                scope: "send frame bytes",
                limit: frame_limit,
                observed: size,
            });
        }
        // Reserve before invoking user encoding; the same guard stays with the
        // queued frame until it is flushed or discarded, rather than ending at enqueue.
        let budget = SendBudget::reserve(self.flow.clone(), size, "send staging")?;
        let bytes = encode()?;
        Ok(Staged {
            bytes,
            payload_len: size,
            _budget: budget,
        })
    }
    async fn ensure_header(&self) -> Result<(), StreamError> {
        if self.flow.header_sent.load(Ordering::Acquire) {
            return Ok(());
        }
        // Reserve before claiming the Header. Dropping a producer while it waits
        // cannot strand the finalizer, and concurrent waiters recheck admission.
        let permit = self.tx.reserve().await.map_err(|_| StreamError::closed())?;
        self.commit_header(|command| {
            permit.send(command);
        })
    }
    fn commit_header(&self, enqueue: impl FnOnce(WriteCommand)) -> Result<(), StreamError> {
        let mut outgoing = self.flow.cx.inner.outgoing.lock();
        if self.flow.header_sent.load(Ordering::Acquire) {
            return Ok(());
        }
        outgoing.header_closed = true;
        let mut frame = self.flow.frame(FrameType::Header, Bytes::new());
        frame.metadata = outgoing.header.clone();
        let bytes = frame.encode(&self.flow.config)?;
        enqueue(WriteCommand {
            bytes: bytes.into(),
            flow: Some(self.flow.clone()),
            finish: None,
            flushed: None,
            control_observer: None,
            reset_id: None,
            _budget: None,
            _lease: None,
        });
        self.flow.header_sent.store(true, Ordering::Release);
        drop(outgoing);
        self.flow
            .with_hooks(|hooks| hooks.on_sent_header(&self.flow.cx, &frame.metadata));
        Ok(())
    }
    async fn send_staged(
        &mut self,
        staged: Result<Staged, StreamError>,
    ) -> Result<(), StreamError> {
        self.check_send()?;
        let staged = staged.inspect_err(|error| {
            self.flow.send_error(error, None);
        })?;
        self.ensure_header().await?;
        let permit = self.tx.reserve().await.map_err(|_| StreamError::closed())?;
        self.commit(staged, |command| permit.send(command))
    }

    fn commit(
        &self,
        staged: Staged,
        enqueue: impl FnOnce(WriteCommand),
    ) -> Result<(), StreamError> {
        self.check_send()?;
        let size = staged.payload_len;
        // A finish racing the synchronous send hook must observe its completed
        // event and counters. Reuse the original observation barrier, with no
        // guard or extra atomic operations on the no-hook path.
        let _observation = self.flow.observe();
        self.flow.cx.send_if_open(|| {
            self.flow.sent_count.fetch_add(1, Ordering::Relaxed);
            self.flow
                .sent_total
                .fetch_add(size as u64, Ordering::Relaxed);
            enqueue(WriteCommand {
                bytes: staged.bytes,
                flow: Some(self.flow.clone()),
                finish: None,
                flushed: None,
                control_observer: None,
                reset_id: None,
                _budget: Some(staged._budget),
                _lease: None,
            });
        })?;
        if _observation.is_some() {
            if let Some(hooks) = &self.flow.hooks {
                hooks.on_send(&self.flow.cx, size);
            }
        }
        Ok(())
    }
    fn check_send(&self) -> Result<(), StreamError> {
        if let Some(error) = self.flow.cx.producer_error() {
            return Err(error);
        }
        if self.flow.send_closed.load(Ordering::Acquire) {
            return Err(StreamError::Protocol("send direction is closed".into()));
        }
        Ok(())
    }
    pub async fn send_raw(&mut self, bytes: Bytes) -> Result<(), StreamError> {
        let staged = self.stage_encoded(bytes.len(), || Ok(bytes));
        self.send_staged(staged).await
    }
    pub async fn close(&mut self, error: Option<StreamError>) -> Result<(), StreamError> {
        if self.flow.send_closed.load(Ordering::Acquire)
            || self.flow.cx.status().is_some()
            || self.flow.cx.cancellation().is_some()
        {
            return Ok(());
        }
        self.ensure_header().await?;
        let permit = match self.tx.reserve().await {
            Ok(permit) => permit,
            Err(_) if self.flow.cx.status().is_some() => return Ok(()),
            Err(_) => return Err(StreamError::closed()),
        };
        let mut outgoing = self.flow.cx.inner.outgoing.lock();
        if self.flow.send_closed.load(Ordering::Acquire)
            || self.flow.cx.status().is_some()
            || self.flow.cx.cancellation().is_some()
        {
            return Ok(());
        }
        let mut frame = self.flow.frame(FrameType::Trailer, Bytes::new());
        frame.metadata = outgoing.trailer.clone();
        if let Some(error) = &error {
            encode_terminal(error, &mut frame)?;
        }
        let bytes = frame.encode(&self.flow.config)?;
        outgoing.trailer = frame.metadata;
        outgoing.trailer_closed = true;
        // The same lock protects DATA admission and the single Trailer commit.
        outgoing.production_closed = true;
        self.flow.send_closed.store(true, Ordering::Release);
        permit.send(WriteCommand {
            bytes: bytes.into(),
            flow: Some(self.flow.clone()),
            // Client half-close also has a flushed Trailer event, but does not
            // finish the bidirectional RPC; the server's Trailer owns that.
            finish: Some(error),
            flushed: None,
            control_observer: None,
            reset_id: None,
            _budget: None,
            _lease: None,
        });
        Ok(())
    }
}

pub(crate) fn encode_terminal(error: &StreamError, frame: &mut Frame) -> Result<(), StreamError> {
    match error {
        StreamError::Biz {
            code,
            message,
            extra,
        } => {
            frame
                .metadata
                .insert(Bytes::from_static(b"biz-status"), code.to_string());
            frame
                .metadata
                .insert(Bytes::from_static(b"biz-message"), message.clone());
            frame.metadata.insert(
                Bytes::from_static(b"biz-extra"),
                sonic_rs::to_string(extra).map_err(|e| StreamError::Protocol(e.to_string()))?,
            );
        }
        error => {
            let exception = match error {
                StreamError::Application(e) => e.clone(),
                StreamError::ProtocolStatus { code, message }
                | StreamError::Canceled { code, message, .. } => {
                    pilota::thrift::ApplicationException::new(
                        pilota::thrift::ApplicationExceptionKind::from_i32(*code),
                        message.clone(),
                    )
                }
                _ => pilota::thrift::ApplicationException::new(
                    pilota::thrift::ApplicationExceptionKind::INTERNAL_ERROR,
                    error.to_string(),
                ),
            };
            let method = std::str::from_utf8(&frame.method)
                .map_err(|_| StreamError::Protocol("method is not UTF-8".into()))?;
            frame.payload = super::encode_exception(method, frame.stream_id, &exception)?;
        }
    }
    Ok(())
}

pub(crate) fn decode_terminal(frame: &Frame) -> Result<Option<StreamError>, StreamError> {
    if !frame.payload.is_empty() {
        return super::decode_exception(frame.payload.clone())
            .map(|e| Some(StreamError::Application(e)))
            .map_err(|error| StreamError::ProtocolStatus {
                code: 12004,
                message: error.to_string(),
            });
    }
    if let Some(code) = frame.metadata.get("biz-status") {
        let Some(code) = std::str::from_utf8(code)
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            return Ok(None);
        };
        if code != 0 {
            let message = frame
                .metadata
                .get("biz-message")
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            let extra = match frame
                .metadata
                .get("biz-extra")
                .filter(|bytes| !bytes.is_empty())
            {
                Some(bytes) => sonic_rs::from_slice::<
                    Option<std::collections::BTreeMap<String, String>>,
                >(bytes)
                .map_err(|error| StreamError::ProtocolStatus {
                    code: 12003,
                    message: format!("malformed biz-extra: {error}"),
                })?
                .unwrap_or_default(),
                None => Default::default(),
            };
            return Ok(Some(StreamError::Biz {
                code,
                message,
                extra,
            }));
        }
    }
    Ok(None)
}

pub(crate) struct RecvCore {
    pub flow: Arc<Flow>,
    pub rx: Arc<ReceiveQueue>,
    pub reported: bool,
}
impl Drop for RecvCore {
    fn drop(&mut self) {
        if !self.flow.client {
            self.flow.discard.store(true, Ordering::Release);
        }
        self.rx.close(true);
        self.flow.finish_receive();
        if self.flow.client && self.flow.cx.status().is_none() {
            self.flow.cx.cancel(StreamError::Canceled {
                cause: CancelCause::Drop,
                code: 12011,
                message: "response stream dropped".into(),
                path: Vec::new(),
            });
        }
    }
}

impl RecvCore {
    pub(crate) fn new(flow: Arc<Flow>, rx: Arc<ReceiveQueue>) -> Self {
        flow.receive_done.store(false, Ordering::Release);
        Self {
            flow,
            rx,
            reported: false,
        }
    }
    fn trailer(&self) -> Option<Metadata> {
        self.flow
            .recv_closed
            .load(Ordering::Acquire)
            .then(|| self.flow.trailer.lock().clone())
    }
    async fn header(&self) -> Result<Metadata, StreamError> {
        loop {
            let notified = self.flow.header_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(header) = self.flow.header.lock().clone() {
                return Ok(header);
            }
            if let Some(status) = self.flow.cx.status() {
                return Err(status.error.unwrap_or_else(|| {
                    StreamError::Protocol("stream ended without response header".into())
                }));
            }
            notified.await;
        }
    }
    fn poll_raw(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<Bytes>, StreamError>> {
        if self.reported {
            return Poll::Ready(Ok(None));
        }
        if let Some(error) = self.flow.cx.cancellation() {
            self.rx.close(true);
            self.reported = true;
            self.flow.finish_receive();
            return Poll::Ready(Err(error));
        }
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(packet)) => Poll::Ready(Ok(Some(packet.bytes))),
            Poll::Ready(None) => {
                self.reported = true;
                let error = self
                    .flow
                    .cx
                    .cancellation()
                    .or_else(|| self.flow.cx.status().and_then(|s| s.error))
                    .or_else(|| self.flow.recv_error.lock().clone());
                self.flow.finish_receive();
                Poll::Ready(error.map_or(Ok(None), Err))
            }
            Poll::Pending => Poll::Pending,
        }
    }
    fn received(&self, size: usize) {
        self.flow.recv_count.fetch_add(1, Ordering::Relaxed);
        self.flow
            .recv_total
            .fetch_add(size as u64, Ordering::Relaxed);
        self.flow
            .with_hooks(|hooks| hooks.on_recv(&self.flow.cx, size));
    }
}

/// A connection-queue slot acquired before encoding a message.
/// Dropping it submits nothing and immediately returns the slot.
pub struct SendReservation<'a, T> {
    core: &'a mut SendCore,
    permit: mpsc::OwnedPermit<WriteCommand>,
    _type: PhantomData<fn(T)>,
}
impl<T: Message> SendReservation<'_, T> {
    /// Encodes synchronously into the reserved slot; no borrow crosses an await.
    pub fn send(self, value: &T) -> Result<(), StreamError> {
        let staged = self.core.stage(value);
        self.core.check_send()?;
        let staged = staged.inspect_err(|error| {
            self.core.flow.send_error(error, None);
        })?;
        self.core.commit(staged, |command| {
            self.permit.send(command);
        })
    }
}

macro_rules! sender {
    ($name:ident, $($field:ident: $ty:ty = $value:expr),*) => {
        pub struct $name<T> {
            pub(crate) core: SendCore,
            $($field: $ty,)*
            _type: PhantomData<fn(T)>,
        }
        impl<T> $name<T> {
            pub(crate) fn new(core: SendCore) -> Self {
                Self {
                    core,
                    $($field: $value,)*
                    _type: PhantomData,
                }
            }
            pub fn context(&self) -> &StreamContext {
                &self.core.flow.cx
            }
            pub async fn close_send(&mut self) -> Result<(), StreamError> {
                self.core.close(None).await
            }
            pub async fn reserve(&mut self) -> Result<SendReservation<'_, T>, StreamError> {
                self.core.check_send()?;
                self.core.ensure_header().await?;
                let permit = self
                    .core
                    .tx
                    .clone()
                    .reserve_owned()
                    .await
                    .map_err(|_| StreamError::closed())?;
                self.core.check_send()?;
                Ok(SendReservation {
                    core: &mut self.core,
                    permit,
                    _type: PhantomData,
                })
            }
            pub fn send<'a>(
                &'a mut self,
                value: &T,
            ) -> impl Future<Output = Result<(), StreamError>> + Send + use<'a, T>
            where
                T: Message,
            {
                let encoded = self.core.stage(value);
                self.core.send_staged(encoded)
            }
        }
    };
}
use std::future::Future;
sender!(RequestSender,);
sender!(ResponseSender, pending: Option<PollSend> = None);

impl<T> Drop for RequestSender<T> {
    fn drop(&mut self) {
        if !self.core.flow.send_closed.load(Ordering::Acquire)
            && self.core.flow.cx.status().is_none()
        {
            self.core.flow.cx.cancel(StreamError::Canceled {
                cause: CancelCause::Drop,
                code: 12011,
                message: "request sender dropped".into(),
                path: Vec::new(),
            });
        }
    }
}
struct PollSend {
    sender: tokio_util::sync::PollSender<WriteCommand>,
    staged: Option<Staged>,
}

impl<T> ResponseSender<T> {
    pub(super) fn prepare_shared(&mut self, value: &T) -> Result<(), StreamError>
    where
        T: Message,
    {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.staged.is_some())
        {
            self.abort_shared();
        }
        let staged = self.core.stage(value).inspect_err(|error| {
            self.core.flow.send_error(error, None);
        })?;
        let pending = self.pending.get_or_insert_with(|| PollSend {
            sender: tokio_util::sync::PollSender::new(self.core.tx.clone()),
            staged: None,
        });
        pending.staged = Some(staged);
        Ok(())
    }
    pub(super) fn poll_shared(&mut self, task: &mut Context<'_>) -> Poll<Result<(), StreamError>> {
        self.core.check_send()?;
        let pending = self.pending.as_mut().expect("send was prepared");
        if !self.core.flow.header_sent.load(Ordering::Acquire) {
            std::task::ready!(pending.sender.poll_reserve(task))
                .map_err(|_| StreamError::closed())?;
            self.core.commit_header(|command| {
                assert!(
                    pending.sender.send_item(command).is_ok(),
                    "reserved Header slot"
                );
            })?;
        }
        std::task::ready!(pending.sender.poll_reserve(task)).map_err(|_| StreamError::closed())?;
        let staged = pending.staged.take().expect("send polled after completion");
        Poll::Ready(self.core.commit(staged, |command| {
            assert!(
                pending.sender.send_item(command).is_ok(),
                "reserved DATA slot"
            );
        }))
    }
    pub(super) fn abort_shared(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.sender.abort_send();
            pending.staged = None;
        }
    }
    pub fn set_trailer(&mut self, metadata: Metadata) -> Result<(), StreamError> {
        if self.core.flow.send_closed.load(Ordering::Acquire)
            || self.core.flow.cx.status().is_some()
        {
            return Err(StreamError::Protocol("trailer already committed".into()));
        }
        self.core.flow.cx.set_outgoing_metadata(metadata, true)
    }
    pub async fn finish(&mut self, result: Result<(), StreamError>) -> Result<(), StreamError> {
        self.core.close(result.err()).await
    }
    pub fn set_header(&mut self, metadata: Metadata) -> Result<(), StreamError> {
        if self.core.flow.header_sent.load(Ordering::Acquire) {
            return Err(StreamError::Protocol("header already sent".into()));
        }
        self.core.flow.cx.set_outgoing_metadata(metadata, false)
    }
    pub async fn send_header(&mut self) -> Result<(), StreamError> {
        self.core.ensure_header().await
    }
}
macro_rules! receiver {
    ($name:ident) => {
        pub struct $name<T> {
            pub(crate) core: RecvCore,
            _type: PhantomData<fn() -> T>,
        }
        impl<T> $name<T> {
            pub(crate) fn new(core: RecvCore) -> Self {
                Self {
                    core,
                    _type: PhantomData,
                }
            }
            pub fn context(&self) -> &StreamContext {
                &self.core.flow.cx
            }
            pub fn status(&self) -> Option<StreamStatus> {
                self.core.flow.cx.status()
            }
            pub async fn header(&self) -> Result<Metadata, StreamError> {
                self.core.header().await
            }
            pub fn trailer(&self) -> Option<Metadata> {
                self.core.trailer()
            }

            /// A per-message wait timeout leaves the stream and queued messages usable.
            pub async fn recv_with_timeout_policy(
                &mut self,
                timeout: std::time::Duration,
                policy: super::RecvTimeoutPolicy,
            ) -> Result<Option<T>, StreamError>
            where
                T: Message,
            {
                let result = self.recv_with_timeout(timeout).await;
                if matches!(result, Err(StreamError::RecvTimeout))
                    && policy == super::RecvTimeoutPolicy::Cancel
                {
                    self.core.flow.cx.cancel(StreamError::Canceled {
                        cause: CancelCause::Deadline,
                        code: 12014,
                        message: "stream receive timeout".into(),
                        path: Vec::new(),
                    });
                }
                result
            }
            pub async fn recv_with_timeout(
                &mut self,
                timeout: std::time::Duration,
            ) -> Result<Option<T>, StreamError>
            where
                T: Message,
            {
                tokio::time::timeout(timeout, self.recv())
                    .await
                    .map_err(|_| StreamError::RecvTimeout)?
            }
            pub async fn recv(&mut self) -> Result<Option<T>, StreamError>
            where
                T: Message,
            {
                futures::future::poll_fn(|cx| Pin::new(&mut *self).poll_next(cx))
                    .await
                    .transpose()
            }
        }
        impl<T: Message> Stream for $name<T> {
            type Item = Result<T, StreamError>;
            fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
                let this = self.get_mut();
                match this.core.poll_raw(cx) {
                    Poll::Ready(Ok(Some(bytes))) => {
                        let len = bytes.len();
                        let result = decode_message(bytes);
                        match &result {
                            Ok(_) => this.core.received(len),
                            Err(error) => {
                                this.core.reported = true;
                                this.core.rx.close(true);
                                this.core.flow.codec_error(
                                    super::IoDirection::Recv,
                                    super::IoPhase::Decode,
                                    Some(len),
                                    error.clone(),
                                );
                                this.core.flow.finish_receive();
                            }
                        }
                        Poll::Ready(Some(result))
                    }
                    Poll::Ready(Ok(None)) => Poll::Ready(None),
                    Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    };
}
receiver!(RequestStream);
receiver!(ResponseStream);
impl<T> Drop for RequestStream<T> {
    fn drop(&mut self) {
        self.core.flow.discard.store(true, Ordering::Release);
        self.core.rx.close(true);
    }
}
impl<T> Drop for ResponseStream<T> {
    fn drop(&mut self) {
        if self.core.flow.cx.status().is_none() {
            self.core.flow.cx.cancel(StreamError::Canceled {
                cause: CancelCause::Drop,
                code: 12011,
                message: "response stream dropped".into(),
                path: Vec::new(),
            });
        }
    }
}
pub struct BidiStreaming<Req, Resp> {
    pub(crate) tx: RequestSender<Req>,
    pub(crate) rx: ResponseStream<Resp>,
}
impl<Req, Resp> BidiStreaming<Req, Resp> {
    pub fn into_split(self) -> (RequestSender<Req>, ResponseStream<Resp>) {
        (self.tx, self.rx)
    }
}
impl<Req: Message, Resp: Message> BidiStreaming<Req, Resp> {
    pub fn send<'a>(
        &'a mut self,
        value: &Req,
    ) -> impl Future<Output = Result<(), StreamError>> + Send + use<'a, Req, Resp> {
        self.tx.send(value)
    }
    pub async fn recv(&mut self) -> Result<Option<Resp>, StreamError> {
        self.rx.recv().await
    }
    pub async fn close_send(&mut self) -> Result<(), StreamError> {
        self.tx.close_send().await
    }
    pub fn context(&self) -> &StreamContext {
        self.rx.context()
    }
}
pub struct ClientStreaming<Req, Resp> {
    pub(crate) inner: BidiStreaming<Req, Resp>,
}
impl<Req: Message, Resp: Message> ClientStreaming<Req, Resp> {
    pub fn send<'a>(
        &'a mut self,
        value: &Req,
    ) -> impl Future<Output = Result<(), StreamError>> + Send + use<'a, Req, Resp> {
        self.inner.tx.send(value)
    }
    pub async fn close_and_recv(mut self) -> Result<Resp, StreamError> {
        self.close_and_recv_ref().await
    }
    /// Finish the upload while retaining access to response metadata.
    pub async fn close_and_recv_ref(&mut self) -> Result<Resp, StreamError> {
        let validation = self.inner.rx.begin_receive_validation()?;
        self.inner.tx.close_send().await?;
        let Some(response) = self.inner.rx.recv().await? else {
            let error = StreamError::Protocol("expected one response, got zero".into());
            validation.reject(error.clone(), None);
            return Err(error);
        };
        if self.inner.rx.recv().await?.is_some() {
            let error = StreamError::Protocol("expected one response, got multiple".into());
            validation.reject(error.clone(), None);
            return Err(error);
        }
        Ok(response)
    }
    pub fn context(&self) -> &StreamContext {
        self.inner.rx.context()
    }
    pub async fn header(&self) -> Result<Metadata, StreamError> {
        self.inner.rx.header().await
    }
    pub fn trailer(&self) -> Option<Metadata> {
        self.inner.rx.trailer()
    }
    pub fn into_split(self) -> (RequestSender<Req>, ResponseStream<Resp>) {
        self.inner.into_split()
    }
}
pub type ServerStreaming<T> = ResponseStream<T>;

pub struct OpenedStream {
    pub(crate) send: SendCore,
    pub(crate) recv: RecvCore,
}
impl OpenedStream {
    pub fn into_raw_split(self) -> (RawSender, RawStream) {
        (RawSender { core: self.send }, RawStream { core: self.recv })
    }
    pub(crate) fn set_lease(&mut self, lease: Box<dyn Send + Sync>) {
        let mut slot = self.send.flow.lease.lock();
        if self.send.flow.cx.status().is_none() {
            *slot = Some(lease);
        }
    }
    pub fn context(&self) -> &StreamContext {
        &self.send.flow.cx
    }
    pub fn into_bidi<Req, Resp>(self) -> BidiStreaming<Req, Resp> {
        BidiStreaming {
            tx: RequestSender::new(self.send),
            rx: ResponseStream::new(self.recv),
        }
    }
    pub fn into_client<Req, Resp>(self) -> ClientStreaming<Req, Resp> {
        ClientStreaming {
            inner: self.into_bidi(),
        }
    }
    pub fn into_server<Req, Resp>(self) -> (RequestStream<Req>, ResponseSender<Resp>) {
        (
            RequestStream::new(self.recv),
            ResponseSender::new(self.send),
        )
    }
    /// Constructs the common business streams directly over this native flow.
    pub fn into_shared<Req, Resp>(
        self,
    ) -> (
        volo::streaming::RequestStream<Req>,
        volo::streaming::ResponseSender<Resp>,
    )
    where
        Req: Message + 'static,
        Resp: Message + 'static,
    {
        use futures::StreamExt;
        let context = self.context().shared_context();
        let (input, output) = self.into_server::<Req, Resp>();
        (
            volo::streaming::RequestStream::new(
                context.clone(),
                input.map(|result| result.map_err(Into::into)),
            ),
            volo::streaming::ResponseSender::new(context, output),
        )
    }
    pub async fn send_raw(&mut self, bytes: Bytes) -> Result<(), StreamError> {
        self.send.send_raw(bytes).await
    }
    pub async fn recv_raw(&mut self) -> Result<Option<Bytes>, StreamError> {
        let bytes = futures::future::poll_fn(|cx| self.recv.poll_raw(cx)).await?;
        if let Some(bytes) = &bytes {
            self.recv.received(bytes.len());
        }
        Ok(bytes)
    }
    pub async fn close_send(&mut self) -> Result<(), StreamError> {
        self.send.close(None).await
    }
}

pub(crate) fn reserve_bytes(
    counter: &AtomicUsize,
    bytes: usize,
    limit: usize,
) -> Result<usize, usize> {
    let mut old = counter.load(Ordering::Relaxed);
    loop {
        let Some(next) = old.checked_add(bytes).filter(|n| *n <= limit) else {
            return Err(old);
        };
        match counter.compare_exchange_weak(old, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(value) => return Ok(value),
            Err(value) => old = value,
        }
    }
}

/// Unwrapped Thrift struct bytes. This does not add a Thrift binary length field.
pub struct RawSender {
    core: SendCore,
}
impl RawSender {
    pub fn set_header(&mut self, metadata: Metadata) -> Result<(), StreamError> {
        if self.core.flow.header_sent.load(Ordering::Acquire) {
            return Err(StreamError::Protocol("header already sent".into()));
        }
        self.core.flow.cx.set_outgoing_metadata(metadata, false)
    }
    pub fn set_trailer(&mut self, metadata: Metadata) -> Result<(), StreamError> {
        if self.core.flow.send_closed.load(Ordering::Acquire)
            || self.core.flow.cx.status().is_some()
        {
            return Err(StreamError::Protocol("trailer already committed".into()));
        }
        self.core.flow.cx.set_outgoing_metadata(metadata, true)
    }
    pub async fn send_header(&mut self) -> Result<(), StreamError> {
        self.core.ensure_header().await
    }
    pub fn context(&self) -> &StreamContext {
        &self.core.flow.cx
    }
    pub async fn send(&mut self, bytes: Bytes) -> Result<(), StreamError> {
        self.core.send_raw(bytes).await
    }
    pub async fn close_send(&mut self) -> Result<(), StreamError> {
        self.core.close(None).await
    }
    pub async fn finish(&mut self, result: Result<(), StreamError>) -> Result<(), StreamError> {
        self.core.close(result.err()).await
    }
}
impl Drop for RawSender {
    fn drop(&mut self) {
        if self.core.flow.client
            && !self.core.flow.send_closed.load(Ordering::Acquire)
            && self.core.flow.cx.status().is_none()
        {
            self.core.flow.cx.cancel(StreamError::Canceled {
                cause: CancelCause::Drop,
                code: 12011,
                message: "raw request sender dropped".into(),
                path: Vec::new(),
            });
        }
    }
}
/// Defers final receive observation while an adapter validates its response.
///
/// Obtain this before the first receive. Protocol termination, pool release and
/// child cancellation remain immediate; only the final hook waits for Drop.
#[must_use = "keep the validation guard until response validation completes"]
struct ReceiveValidation {
    flow: Arc<Flow>,
}
impl ReceiveValidation {
    fn new(flow: Arc<Flow>) -> Result<Self, StreamError> {
        flow.pending_observation.fetch_add(1, Ordering::AcqRel);
        let validation = Self { flow };
        if validation.flow.observation_done.load(Ordering::Acquire) {
            return Err(StreamError::configuration(
                "receive validation must begin before final observation",
            ));
        }
        Ok(validation)
    }
    /// Ends reception with a local shape/codec error, preserving wire status.
    /// Codec adapters that already called `codec_error` should simply drop
    /// the guard instead, so the I/O event is not reported twice.
    pub fn reject(self, error: StreamError, size: Option<usize>) {
        self.flow.codec_error(
            super::IoDirection::Recv,
            super::IoPhase::Decode,
            size,
            error,
        );
        self.flow.inbound.close(true);
        self.flow.finish_receive();
    }
}
impl Drop for ReceiveValidation {
    fn drop(&mut self) {
        if self.flow.pending_observation.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.flow.publish_observation();
        }
    }
}
impl<T> ResponseStream<T> {
    fn begin_receive_validation(&self) -> Result<ReceiveValidation, StreamError> {
        ReceiveValidation::new(self.core.flow.clone())
    }
}

pub struct RawStream {
    core: RecvCore,
}
impl RawStream {
    pub async fn header(&self) -> Result<Metadata, StreamError> {
        self.core.header().await
    }
    pub fn trailer(&self) -> Option<Metadata> {
        self.core.trailer()
    }
    pub fn context(&self) -> &StreamContext {
        &self.core.flow.cx
    }
    pub async fn recv(&mut self) -> Result<Option<Bytes>, StreamError> {
        let bytes = futures::future::poll_fn(|cx| self.core.poll_raw(cx)).await?;
        if let Some(bytes) = &bytes {
            self.core.received(bytes.len());
        }
        Ok(bytes)
    }
    pub fn status(&self) -> Option<StreamStatus> {
        self.core.flow.cx.status()
    }
}
impl Drop for RawStream {
    fn drop(&mut self) {
        if self.core.flow.client {
            if self.core.flow.cx.status().is_none() {
                self.core.flow.cx.cancel(StreamError::Canceled {
                    cause: CancelCause::Drop,
                    code: 12011,
                    message: "raw response stream dropped".into(),
                    path: Vec::new(),
                });
            }
        } else {
            self.core.flow.discard.store(true, Ordering::Release);
            self.core.rx.close(true);
        }
    }
}

pub(crate) static SHARED_RECV_BYTES: AtomicUsize = AtomicUsize::new(0);
static SHARED_SEND_BYTES: AtomicUsize = AtomicUsize::new(0);

#[cfg(all(test, feature = "ttstream"))]
mod staging_tests {
    use bytes::Buf;
    use pilota::thrift::{ApplicationException, ApplicationExceptionKind};

    use super::*;
    #[test]
    fn typed_and_raw_staging_retain_payload_storage_and_budget() {
        use volo_gen::thrift_gen::hello::HelloRequest;

        let config = Arc::new(StreamConfig::default());
        let (flow, _) = Flow::new(
            StreamContext::new(1, "S", "M", volo::context::Role::Client),
            config.clone(),
            None,
            true,
            Arc::default(),
            Arc::default(),
        );
        let (tx, _rx) = mpsc::channel(1);
        let core = SendCore {
            flow: flow.clone(),
            tx,
            template: FrameTemplate::data(Bytes::from_static(b"M"), &config).unwrap(),
        };
        for size in [0, 128, 4096, 65536] {
            let name = pilota::FastStr::from("x".repeat(size));
            let hello = pilota::FastStr::from("y".repeat(size));
            let message = HelloRequest {
                name: name.clone(),
                hello: Some(hello.clone()),
                ..Default::default()
            };
            let staged = core.stage(&message).unwrap();
            drop(message);
            assert_eq!(flow.send_bytes.load(Ordering::Relaxed), staged.payload_len);
            if size >= 4096 {
                let mut slices = [std::io::IoSlice::new(&[]); 8];
                let count = staged.bytes.chunks_vectored(&mut slices);
                for expected in [&name, &hello] {
                    assert!(slices[..count].iter().any(|slice| slice.as_ptr() == expected.as_ptr() && slice.len() == size), "encoded FastStr storage was copied");
                }
            } else {
                assert!(matches!(staged.bytes, FrameBytes::Contiguous(_)));
            }
            let frame = Frame::decode(staged.bytes.into_bytes(), &config).unwrap();
            let decoded = decode_message::<HelloRequest>(frame.payload).unwrap();
            assert_eq!(decoded.name, name);
            assert_eq!(decoded.hello, Some(hello));
            assert_eq!(flow.send_bytes.load(Ordering::Relaxed), staged.payload_len);
            drop(staged._budget);
            assert_eq!(flow.send_bytes.load(Ordering::Relaxed), 0);

            let original = Bytes::from(vec![b'r'; size]);
            let raw = core.stage_encoded(size, || Ok(original.clone())).unwrap();
            if size != 0 {
                let mut slices = [std::io::IoSlice::new(&[]); 4];
                let count = raw.bytes.chunks_vectored(&mut slices);
                assert!(
                    slices[..count]
                        .iter()
                        .any(|slice| slice.as_ptr() == original.as_ptr() && slice.len() == size)
                );
            }
            drop(raw);
            assert_eq!(flow.send_bytes.load(Ordering::Relaxed), 0);
        }
        for actual in [3, 5] {
            assert!(
                core.stage_encoded(4, || Ok(Bytes::from(vec![0; actual])))
                    .is_err()
            );
            assert_eq!(flow.send_bytes.load(Ordering::Relaxed), 0);
        }
    }

    #[tokio::test]
    async fn concurrent_half_closes_enqueue_exactly_one_trailer() {
        let config = Arc::new(StreamConfig::default());
        let cx = StreamContext::new(1, "S", "M", volo::context::Role::Client);
        let (flow, _) = Flow::new(
            cx,
            config.clone(),
            None,
            true,
            Arc::default(),
            Arc::default(),
        );
        let (tx, mut rx) = mpsc::channel(1);
        tx.send(WriteCommand {
            bytes: Bytes::new().into(),
            flow: None,
            finish: None,
            flushed: None,
            control_observer: None,
            reset_id: None,
            _budget: None,
            _lease: None,
        })
        .await
        .unwrap();
        let mut first = SendCore {
            flow: flow.clone(),
            tx: tx.clone(),
            template: FrameTemplate::data(Bytes::from_static(b"M"), &config).unwrap(),
        };
        let mut second = SendCore {
            flow,
            tx,
            template: first.template.clone(),
        };
        let first = first.close(None);
        let second = second.close(None);
        tokio::pin!(first, second);
        assert!(futures::poll!(&mut first).is_pending());
        assert!(futures::poll!(&mut second).is_pending());
        rx.recv().await.unwrap();
        first.await.unwrap();
        let trailer = rx.recv().await.unwrap();
        assert_eq!(
            Frame::decode(trailer.bytes.into_bytes(), &config)
                .unwrap()
                .kind,
            FrameType::Trailer
        );
        assert!(
            trailer.finish.is_some(),
            "client half-close needs a flushed hook event"
        );
        second.await.unwrap();
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn send_observation_precedes_a_racing_finish_and_keeps_committed_counts() {
        #[derive(Default)]
        struct Observer {
            flow: Mutex<std::sync::Weak<Flow>>,
            events: Mutex<Vec<&'static str>>,
        }
        impl StreamHook for Observer {
            fn on_send(&self, _: &StreamContext, _: usize) {
                self.events.lock().push("send-start");
                self.flow.lock().upgrade().unwrap().finish(None);
                self.events.lock().push("send-end");
            }
            fn on_finish(&self, _: &StreamContext, status: &StreamStatus) {
                assert_eq!(status.sent_messages, 1);
                self.events.lock().push("finish");
            }
        }
        let observer = Arc::new(Observer::default());
        let mut hooks = HookChain::default();
        hooks.push(observer.clone());
        let config = Arc::new(StreamConfig::default());
        let cx = StreamContext::new(1, "S", "M", volo::context::Role::Server);
        let (flow, _rx) = Flow::new(
            cx.clone(),
            config.clone(),
            Some(hooks),
            false,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        *observer.flow.lock() = Arc::downgrade(&flow);
        flow.header_sent.store(true, Ordering::Release);
        let (tx, _writes) = mpsc::channel(1);
        let mut core = SendCore {
            flow,
            tx,
            template: FrameTemplate::data(Bytes::from_static(b"M"), &config).unwrap(),
        };
        core.send_raw(Bytes::from_static(b"\0")).await.unwrap();
        assert_eq!(
            *observer.events.lock(),
            ["send-start", "send-end", "finish"]
        );
        assert_eq!(cx.status().unwrap().sent_messages, 1);
    }

    #[tokio::test]
    async fn common_producer_completion_wakes_without_queue_capacity_and_releases_its_header_claim()
    {
        let config = Arc::new(StreamConfig::default());
        let cx = StreamContext::new(1, "S", "M", volo::context::Role::Server);
        let (flow, _rx) = Flow::new(
            cx.clone(),
            config.clone(),
            None,
            false,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        let (tx, mut writes) = mpsc::channel(1);
        tx.try_send(WriteCommand {
            bytes: Bytes::new().into(),
            flow: None,
            finish: None,
            flushed: None,
            control_observer: None,
            reset_id: None,
            _budget: None,
            _lease: None,
        })
        .unwrap();
        let core = || SendCore {
            flow: flow.clone(),
            tx: tx.clone(),
            template: FrameTemplate::data(Bytes::from_static(b"M"), &config).unwrap(),
        };
        let mut sender = volo::streaming::ResponseSender::new(
            cx.shared_context(),
            ResponseSender::<ApplicationException>::new(core()),
        );
        let message =
            ApplicationException::new(ApplicationExceptionKind::INTERNAL_ERROR, "pending");
        {
            let sending = sender.send(&message);
            tokio::pin!(sending);
            assert!(futures::poll!(&mut sending).is_pending());
            assert!(flow.send_bytes.load(Ordering::Relaxed) > 0);
            cx.close_production();
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(1), sending)
                    .await
                    .unwrap()
                    .is_err()
            );
        }
        assert_eq!(flow.send_bytes.load(Ordering::Relaxed), 0);
        assert!(cx.status().is_none());
        assert!(cx.cancellation().is_none());
        assert_eq!(
            writes.len(),
            1,
            "the receiver remains owned and the queue is still full"
        );
        drop(writes.recv().await.unwrap());
        let mut finalizer = core();
        let closing = finalizer.close(None);
        tokio::pin!(closing);
        assert!(futures::poll!(&mut closing).is_pending());
        let header = writes.recv().await.unwrap();
        assert!(header.finish.is_none());
        closing.await.unwrap();
        let trailer = writes.recv().await.unwrap();
        assert!(matches!(trailer.finish, Some(None)));
        assert!(writes.try_recv().is_err());
    }

    #[tokio::test]
    async fn concurrent_first_send_and_finish_enqueue_one_header() {
        let config = Arc::new(StreamConfig::default());
        let cx = StreamContext::new(1, "S", "M", volo::context::Role::Server);
        let (flow, _rx) = Flow::new(
            cx,
            config.clone(),
            None,
            false,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        let (tx, mut writes) = mpsc::channel(1);
        tx.try_send(WriteCommand {
            bytes: Bytes::new().into(),
            flow: None,
            finish: None,
            flushed: None,
            control_observer: None,
            reset_id: None,
            _budget: None,
            _lease: None,
        })
        .unwrap();
        let core = || SendCore {
            flow: flow.clone(),
            tx: tx.clone(),
            template: FrameTemplate::data(Bytes::from_static(b"M"), &config).unwrap(),
        };
        let producer = core();
        let finish = core();
        let first = producer.ensure_header();
        let last = finish.ensure_header();
        tokio::pin!(first, last);
        assert!(futures::poll!(&mut first).is_pending());
        assert!(futures::poll!(&mut last).is_pending());
        drop(writes.recv().await.unwrap());
        first.await.unwrap();
        drop(writes.recv().await.unwrap());
        last.await.unwrap();
        assert!(
            writes.try_recv().is_err(),
            "HEADER must be enqueued exactly once"
        );
    }

    #[tokio::test]
    async fn handler_completion_rejects_a_waiting_producer_before_the_trailer() {
        let config = Arc::new(StreamConfig::default());
        let cx = StreamContext::new(1, "S", "M", volo::context::Role::Server);
        let (flow, _rx) = Flow::new(
            cx.clone(),
            config.clone(),
            None,
            false,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        flow.header_sent.store(true, Ordering::Release);
        let (tx, mut writes) = mpsc::channel(1);
        tx.try_send(WriteCommand {
            bytes: Bytes::new().into(),
            flow: None,
            finish: None,
            flushed: None,
            control_observer: None,
            reset_id: None,
            _budget: None,
            _lease: None,
        })
        .unwrap();
        let mut core = SendCore {
            flow: flow.clone(),
            tx,
            template: FrameTemplate::data(Bytes::from_static(b"M"), &config).unwrap(),
        };
        {
            let sending = core.send_raw(Bytes::from_static(b"\0"));
            tokio::pin!(sending);
            assert!(futures::poll!(&mut sending).is_pending());
            assert_eq!(flow.send_bytes.load(Ordering::Relaxed), 1);
            cx.close_production();
            assert!(cx.status().is_none());
            assert!(cx.cancellation().is_none());
            assert!(cx.set_outgoing_metadata(Metadata::default(), true).is_err());
            drop(writes.recv().await.unwrap());
            assert!(sending.await.is_err());
        }
        assert_eq!(flow.send_bytes.load(Ordering::Relaxed), 0);
        assert!(core.send_raw(Bytes::from_static(b"\0")).await.is_err());
        let permit = core.tx.clone().reserve_owned().await.unwrap();
        let reserved = SendReservation::<ApplicationException> {
            core: &mut core,
            permit,
            _type: PhantomData,
        };
        assert!(
            reserved
                .send(&ApplicationException::new(
                    ApplicationExceptionKind::UNKNOWN,
                    "late reserved send"
                ))
                .is_err()
        );
        assert!(
            cx.cancellation().is_none(),
            "late producers cannot replace the handler result"
        );
        assert!(
            writes.try_recv().is_err(),
            "no late DATA may pass the close boundary"
        );
        core.close(None).await.unwrap();
        let trailer = writes.recv().await.unwrap();
        assert!(matches!(trailer.finish, Some(None)));
        assert!(
            cx.status().is_none(),
            "only the original driver commits wire status"
        );
        flow.finish(None);
        assert!(cx.status().unwrap().error.is_none());
    }

    #[test]
    fn raw_conversion_admission_and_unwind_release_the_same_budget() {
        let config = Arc::new(StreamConfig {
            send_staging_bytes: 4,
            ..Default::default()
        });
        let cx = StreamContext::new(1, "S", "M", volo::context::Role::Client);
        let (flow, _rx) = Flow::new(
            cx,
            config.clone(),
            None,
            true,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        let (tx, _writes) = mpsc::channel(1);
        let mut sender = RawSender {
            core: SendCore {
                flow: flow.clone(),
                tx,
                template: FrameTemplate::data(Bytes::from_static(b"M"), &config).unwrap(),
            },
        };
        let called = std::cell::Cell::new(false);
        assert!(matches!(
            sender.core.stage_encoded(5, || {
                called.set(true);
                Ok(Bytes::new())
            }),
            Err(StreamError::ResourceExhausted { .. })
        ));
        assert!(!called.get());
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sender
                .core
                .stage_encoded(4, || panic!("codec panic"))))
            .is_err()
        );
        assert_eq!(flow.send_bytes.load(Ordering::Relaxed), 0);
        let prepared = sender
            .core
            .stage_encoded(4, || Ok(Bytes::from_static(b"data")))
            .unwrap();
        assert_eq!(flow.send_bytes.load(Ordering::Relaxed), 4);
        let future = sender.core.send_staged(Ok(prepared));
        drop(future);
        assert_eq!(flow.send_bytes.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn queued_data_retains_budget_and_rejection_does_not_cancel() {
        struct Hook(AtomicUsize);
        impl StreamHook for Hook {
            fn on_io_error(&self, _: &StreamContext, _: &super::super::StreamIoError) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let hook = Arc::new(Hook(AtomicUsize::new(0)));
        let config = Arc::new(StreamConfig {
            send_staging_bytes: 8,
            connection_send_bytes: 6,
            ..Default::default()
        });
        let connection = Arc::new(AtomicUsize::new(0));
        let cx = StreamContext::new(1, "S", "M", volo::context::Role::Client);
        let (flow, _) = Flow::new(
            cx,
            config.clone(),
            Some(HookChain::new([hook.clone() as Arc<dyn StreamHook>])),
            true,
            Arc::default(),
            connection.clone(),
        );
        let (tx, mut writes) = mpsc::channel(4);
        let mut sender = RawSender {
            core: SendCore {
                flow: flow.clone(),
                tx,
                template: FrameTemplate::data(Bytes::from_static(b"M"), &config).unwrap(),
            },
        };
        sender.send(Bytes::from_static(b"data")).await.unwrap();
        assert_eq!(flow.send_bytes.load(Ordering::Relaxed), 4);
        assert_eq!(connection.load(Ordering::Relaxed), 4);
        assert!(matches!(
            sender.send(Bytes::from_static(b"data")).await,
            Err(StreamError::ResourceExhausted {
                scope: "connection send bytes",
                ..
            })
        ));
        assert!(flow.cx.cancellation().is_none());
        assert_eq!(hook.0.load(Ordering::Relaxed), 1);
        let command = writes.recv().await.unwrap();
        assert_eq!(flow.cx.resources().send_buffer_bytes, 4);
        drop(command);
        assert_eq!(connection.load(Ordering::Relaxed), 0);
        assert_eq!(flow.cx.resources().send_buffer_bytes, 0);
        assert_eq!(flow.cx.resources().send_high_water_bytes, 4);
    }
    #[test]
    fn shared_send_refusal_rolls_back_flow_and_connection_reservations() {
        let config = Arc::new(StreamConfig {
            shared_send_bytes: 0,
            ..Default::default()
        });
        let connection = Arc::new(AtomicUsize::new(0));
        let cx = StreamContext::new(1, "S", "M", volo::context::Role::Client);
        let (flow, _) = Flow::new(cx, config, None, true, Arc::default(), connection.clone());
        assert!(matches!(
            SendBudget::reserve(flow.clone(), 4, "staging"),
            Err(StreamError::ResourceExhausted {
                scope: "shared send bytes",
                ..
            })
        ));
        assert_eq!(flow.send_bytes.load(Ordering::Relaxed), 0);
        assert_eq!(connection.load(Ordering::Relaxed), 0);
        assert_eq!(flow.cx.resources().send_buffer_bytes, 0);
    }
    #[tokio::test]
    async fn half_close_waiting_for_queue_accepts_remote_normal_completion() {
        let config = Arc::new(StreamConfig::default());
        let cx = StreamContext::new(1, "S", "M", volo::context::Role::Client);
        let (flow, _rx) = Flow::new(
            cx,
            config.clone(),
            None,
            true,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        let (tx, writes) = mpsc::channel(1);
        tx.try_send(WriteCommand {
            bytes: Bytes::new().into(),
            flow: None,
            finish: None,
            flushed: None,
            control_observer: None,
            reset_id: None,
            _budget: None,
            _lease: None,
        })
        .unwrap();
        let mut core = SendCore {
            flow: flow.clone(),
            tx,
            template: FrameTemplate::data(Bytes::from_static(b"M"), &config).unwrap(),
        };
        let closing = core.close(None);
        tokio::pin!(closing);
        assert!(futures::poll!(&mut closing).is_pending());
        flow.finish(None);
        drop(writes);
        closing.await.unwrap();
        assert!(flow.cx.status().unwrap().error.is_none());
    }
    #[test]
    fn trailer_biz_parsing_matches_go_invalid_status_and_extra_rules() {
        let mut frame = Frame::new(
            1,
            FrameType::Trailer,
            Bytes::from_static(b"M"),
            Bytes::new(),
        );
        for status in ["not-a-code", "2147483648", "0", ""] {
            frame
                .metadata
                .insert(Bytes::from_static(b"biz-status"), status.to_owned());
            frame
                .metadata
                .insert(Bytes::from_static(b"biz-extra"), "invalid".to_owned());
            assert!(decode_terminal(&frame).unwrap().is_none());
        }
        frame
            .metadata
            .insert(Bytes::from_static(b"biz-status"), "-1".to_owned());
        assert!(matches!(
            decode_terminal(&frame),
            Err(StreamError::ProtocolStatus { code: 12003, .. })
        ));
        frame
            .metadata
            .insert(Bytes::from_static(b"biz-extra"), "null".to_owned());
        assert!(
            matches!(decode_terminal(&frame).unwrap(),Some(StreamError::Biz {code:-1,extra,..}) if extra.is_empty())
        );
        frame.payload = Bytes::from_static(b"bad-exception");
        assert!(matches!(
            decode_terminal(&frame),
            Err(StreamError::ProtocolStatus { code: 12004, .. })
        ));
    }
}

#[cfg(all(test, feature = "ttstream"))]
mod observation_tests {
    use pilota::thrift::{ApplicationException, ApplicationExceptionKind};

    use super::*;
    #[derive(Default)]
    struct Events {
        order: Mutex<Vec<&'static str>>,
        status: Mutex<Vec<StreamStatus>>,
    }
    impl StreamHook for Events {
        fn on_io_error(&self, _: &StreamContext, _: &super::super::StreamIoError) {
            self.order.lock().push("io_error");
        }
        fn on_recv(&self, _: &StreamContext, _: usize) {
            self.order.lock().push("recv");
        }
        fn on_finish(&self, _: &StreamContext, status: &StreamStatus) {
            self.order.lock().push("finish");
            self.status.lock().push(status.clone());
        }
    }
    fn observed_flow(client: bool) -> (Arc<Flow>, Arc<Events>) {
        let events = Arc::new(Events::default());
        let (flow, _) = Flow::new(
            StreamContext::new(
                1,
                "S",
                "M",
                if client {
                    volo::context::Role::Client
                } else {
                    volo::context::Role::Server
                },
            ),
            Arc::new(StreamConfig::default()),
            Some(HookChain::new([events.clone() as Arc<dyn StreamHook>])),
            client,
            Arc::default(),
            Arc::default(),
        );
        (flow, events)
    }
    #[test]
    fn reset_observation_waits_for_control_flush_with_final_resources() {
        let (flow, events) = observed_flow(false);
        let mut observation = ControlObservation::flow(flow.clone());
        // A deterministic queued interval avoids relying on clock resolution.
        observation.queued -= std::time::Duration::from_millis(1);
        let error = StreamError::ResourceExhausted {
            scope: "stream bytes",
            limit: 8,
            observed: 9,
        };
        flow.cx.cancel(error.clone());
        flow.finish(Some(error));
        assert!(flow.cx.status().is_some());
        assert!(events.status.lock().is_empty());
        observation.flushed();
        drop(observation);
        flow.finish(None);
        let status = events.status.lock();
        assert_eq!(status.len(), 1);
        assert!(status[0].resources.control_queue_delay_nanos >= 1_000_000);
        assert_eq!(*events.order.lock(), vec!["finish"]);
    }
    #[test]
    fn discarded_control_still_finishes_observation_once() {
        let (flow, events) = observed_flow(false);
        let observation = ControlObservation::flow(flow.clone());
        flow.finish(Some(StreamError::closed()));
        assert!(events.status.lock().is_empty());
        drop(observation);
        flow.finish(None);
        assert_eq!(*events.order.lock(), vec!["io_error", "finish"]);
        assert_eq!(events.status.lock().len(), 1);
        assert_eq!(
            events.status.lock()[0].resources.control_queue_delay_nanos,
            0
        );
    }
    #[test]
    fn client_observation_waits_for_both_receive_drop_and_control_flush() {
        for flush_first in [false, true] {
            let (flow, events) = observed_flow(true);
            let receiver = ResponseStream::<ApplicationException>::new(RecvCore::new(
                flow.clone(),
                flow.inbound.clone(),
            ));
            let mut observation = ControlObservation::flow(flow.clone());
            flow.cx.cancel(StreamError::closed());
            flow.finish(Some(StreamError::closed()));
            if flush_first {
                observation.flushed();
                drop(observation);
                assert!(events.status.lock().is_empty());
                drop(receiver);
            } else {
                drop(receiver);
                assert!(events.status.lock().is_empty());
                observation.flushed();
                drop(observation);
            }
            assert_eq!(events.status.lock().len(), 1);
        }
    }
    #[test]
    fn rejected_header_observation_waits_for_flush_or_discard() {
        for flush in [false, true] {
            let (flow, events) = observed_flow(false);
            let hooks = flow.hooks.clone().unwrap();
            flow.cx.finish(StreamStatus {
                error: Some(StreamError::closed()),
                ..Default::default()
            });
            let mut observation = ControlObservation::rejected(flow.cx.clone(), hooks);
            assert!(events.status.lock().is_empty());
            if flush {
                observation.flushed();
            }
            drop(observation);
            assert_eq!(events.status.lock().len(), 1);
            assert_eq!(events.order.lock().last(), Some(&"finish"));
        }
    }
    fn buffered() -> (Arc<Flow>, ResponseStream<ApplicationException>, Arc<Events>) {
        let events = Arc::new(Events::default());
        let (flow, rx) = Flow::new(
            StreamContext::new(1, "S", "M", volo::context::Role::Client),
            Arc::new(StreamConfig::default()),
            Some(HookChain::new([events.clone() as Arc<dyn StreamHook>])),
            true,
            Arc::default(),
            Arc::default(),
        );
        let receiver = ResponseStream::new(RecvCore::new(flow.clone(), rx));
        for _ in 0..3 {
            let bytes = crate::streaming::encode_message(&ApplicationException::new(
                ApplicationExceptionKind::INTERNAL_ERROR,
                "message",
            ))
            .unwrap();
            let _budget = ReceiveReservation::new(flow.clone(), bytes.len()).unwrap();
            flow.inbound.push(Packet { bytes, _budget }).unwrap();
        }
        flow.finish(None);
        assert!(flow.cx.status().is_some());
        assert!(events.order.lock().is_empty());
        (flow, receiver, events)
    }
    #[tokio::test]
    async fn client_streaming_zero_or_multiple_responses_finish_as_local_protocol_errors() {
        for count in [0, 1, 2] {
            let (flow, events) = observed_flow(true);
            let receiver = ResponseStream::<ApplicationException>::new(RecvCore::new(
                flow.clone(),
                flow.inbound.clone(),
            ));
            let (writes, _commands) = mpsc::channel(4);
            let sender = RequestSender::<ApplicationException>::new(SendCore {
                flow: flow.clone(),
                tx: writes,
                template: FrameTemplate::data(Bytes::from_static(b"M"), &flow.config).unwrap(),
            });
            for _ in 0..count {
                let bytes = crate::streaming::encode_message(&ApplicationException::new(
                    ApplicationExceptionKind::INTERNAL_ERROR,
                    "value",
                ))
                .unwrap();
                let _budget = ReceiveReservation::new(flow.clone(), bytes.len()).unwrap();
                flow.inbound.push(Packet { bytes, _budget }).unwrap();
            }
            flow.finish(None);
            let stream = ClientStreaming {
                inner: BidiStreaming {
                    tx: sender,
                    rx: receiver,
                },
            };
            let result = stream.close_and_recv().await;
            assert_eq!(result.is_ok(), count == 1);
            let status = events.status.lock();
            assert_eq!(status.len(), 1, "count={count}");
            assert_eq!(
                status[0].error.is_some(),
                count != 1,
                "count={count}: local shape failure must not finish as success"
            );
        }
    }

    #[test]
    fn native_event_gate_orders_finish_after_callbacks_and_rejects_late_events() {
        let (flow, events) = observed_flow(false);
        let (entered, running) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let sending = flow.clone();
        let worker = std::thread::spawn(move || {
            sending.with_hooks(|hooks| {
                entered.send(()).unwrap();
                released.recv().unwrap();
                hooks.on_recv(&sending.cx, 7);
            });
        });
        running.recv().unwrap();
        flow.finish(None);
        assert!(events.status.lock().is_empty());
        release.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(*events.order.lock(), vec!["recv", "finish"]);
        flow.with_hooks(|_| panic!("late callback after native finish"));
        flow.finish(None);
        assert_eq!(events.status.lock().len(), 1);
    }

    #[test]
    fn outer_open_failure_must_not_be_observed_again_when_receiver_drops() {
        let events = Arc::new(Events::default());
        let (flow, queue) = Flow::new(
            StreamContext::new(1, "S", "M", volo::context::Role::Client),
            Arc::new(StreamConfig::default()),
            Some(HookChain::new([events.clone() as Arc<dyn StreamHook>])),
            true,
            Arc::default(),
            Arc::default(),
        );
        let receiver =
            ResponseStream::<ApplicationException>::new(RecvCore::new(flow.clone(), queue));
        let status = StreamStatus {
            error: Some(StreamError::configuration(
                "outer layer rejected opened stream",
            )),
            ..Default::default()
        };
        // OpenBoundary owns failures returned by an outer layer after inner open.
        assert!(flow.cx.finish(status.clone()));
        events.on_finish(&flow.cx, &status);
        drop(receiver);
        flow.finish(None);
        assert_eq!(events.status.lock().len(), 1);
    }

    #[tokio::test]
    async fn queued_data_observation_finishes_after_application_eof_once() {
        let (flow, mut receiver, events) = buffered();
        for _ in 0..3 {
            assert!(receiver.recv().await.unwrap().is_some());
        }
        assert_eq!(*events.order.lock(), vec!["recv", "recv", "recv"]);
        assert!(receiver.recv().await.unwrap().is_none());
        assert!(receiver.recv().await.unwrap().is_none());
        drop(receiver);
        flow.finish(None);
        assert_eq!(*events.order.lock(), vec!["recv", "recv", "recv", "finish"]);
        let status = events.status.lock();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].received_messages, 3);
        assert_eq!(status[0].resources.queued_messages, 0);
    }
    #[tokio::test]
    async fn dropping_receiver_discards_unread_data_before_final_snapshot() {
        let (flow, mut receiver, events) = buffered();
        assert!(receiver.recv().await.unwrap().is_some());
        drop(receiver);
        flow.finish(None);
        assert_eq!(*events.order.lock(), vec!["recv", "finish"]);
        let status = events.status.lock();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].received_messages, 1);
        assert_eq!(status[0].resources.queued_bytes, 0);
    }
    #[tokio::test]
    async fn decode_failure_after_wire_success_finishes_observation_with_error() {
        let events = Arc::new(Events::default());
        let (flow, rx) = Flow::new(
            StreamContext::new(1, "S", "M", volo::context::Role::Client),
            Arc::new(StreamConfig::default()),
            Some(HookChain::new([events.clone() as Arc<dyn StreamHook>])),
            true,
            Arc::default(),
            Arc::default(),
        );
        let mut receiver =
            ResponseStream::<ApplicationException>::new(RecvCore::new(flow.clone(), rx));
        let bytes = Bytes::from_static(&[0xff]);
        let _budget = ReceiveReservation::new(flow.clone(), bytes.len()).unwrap();
        flow.inbound.push(Packet { bytes, _budget }).unwrap();
        flow.finish(None);
        assert!(receiver.recv().await.is_err());
        drop(receiver);
        let status = events.status.lock();
        assert_eq!(status.len(), 1);
        assert!(matches!(status[0].error, Some(StreamError::Protocol(_))));
        assert!(flow.cx.status().unwrap().error.is_none());
    }
}
