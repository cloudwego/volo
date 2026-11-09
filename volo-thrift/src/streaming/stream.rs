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
use pilota::thrift::Message;
use tokio::sync::{Notify, mpsc};

use super::{
    CancelCause, HookChain, Metadata, StreamConfig, StreamContext, StreamError, StreamHook,
    StreamStatus, decode_message, encode_message,
};
use crate::transport::ttstream::frame::{Frame, FrameTemplate, FrameType};

pub(crate) struct Flow {
    pub cx: StreamContext,
    pub config: Arc<StreamConfig>,
    pub hooks: Option<HookChain>,
    pub inbound: Arc<ReceiveQueue>,
    pub header: Mutex<Option<Metadata>>,
    pub trailer: Mutex<Metadata>,
    pub recv_error: Mutex<Option<StreamError>>,
    pub pending_trailer: Mutex<Metadata>,
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
    pub pending_header: Mutex<Metadata>,
    pub client: bool,
    pub lease: Mutex<Option<Box<dyn Send + Sync>>>,
    receive_done: AtomicBool,
    observation_done: AtomicBool,
    owns_terminal: AtomicBool,
    wire_finished: AtomicBool,
    pending_observation: AtomicUsize,
    observation_error: Mutex<Option<StreamError>>,
}

impl Flow {
    pub fn new(
        cx: StreamContext,
        config: Arc<StreamConfig>,
        hooks: Option<HookChain>,
        client: bool,
        connection_recv_bytes: Arc<AtomicUsize>,
        connection_send_bytes: Arc<AtomicUsize>,
    ) -> (Arc<Self>, Arc<ReceiveQueue>) {
        let hooks = cx.inner.hooks.get().cloned().or(hooks);
        let queue = Arc::new(ReceiveQueue::new(config.recv_messages));
        (
            Arc::new(Self {
                cx,
                config,
                hooks,
                inbound: queue.clone(),
                header: Mutex::new(None),
                trailer: Mutex::default(),
                recv_error: Mutex::new(None),
                pending_trailer: Mutex::default(),
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
                pending_header: Mutex::default(),
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
        if self.observation_done.swap(true, Ordering::AcqRel) {
            return;
        }
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
        if let Some(hooks) = &self.hooks {
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
        }
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
            self.conversion_error(
                super::IoDirection::Send,
                super::IoPhase::Encode,
                size,
                error.clone(),
            );
        }
    }
    fn conversion_error(
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
            Bytes::copy_from_slice(self.cx.method().as_bytes()),
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
    packets: Mutex<VecDeque<Packet>>,
    closed: AtomicBool,
    capacity: usize,
    waker: futures::task::AtomicWaker,
}
impl ReceiveQueue {
    fn new(capacity: usize) -> Self {
        Self {
            packets: Mutex::default(),
            closed: AtomicBool::new(false),
            capacity,
            waker: futures::task::AtomicWaker::new(),
        }
    }
    pub fn is_full(&self) -> bool {
        self.packets.lock().len() >= self.capacity
    }
    pub fn push(&self, packet: Packet) -> Result<(), ReceivePushError> {
        let mut packets = self.packets.lock();
        if self.closed.load(Ordering::Acquire) {
            return Err(ReceivePushError::Closed);
        }
        if packets.len() >= self.capacity {
            return Err(ReceivePushError::Full);
        }
        packets.push_back(packet);
        drop(packets);
        self.waker.wake();
        Ok(())
    }
    pub fn close(&self, discard: bool) {
        self.closed.store(true, Ordering::Release);
        if discard {
            let packets = std::mem::take(&mut *self.packets.lock());
            drop(packets);
        }
        self.waker.wake();
    }
    fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Option<Packet>> {
        self.waker.register(cx.waker());
        let mut packets = self.packets.lock();
        if let Some(packet) = packets.pop_front() {
            return Poll::Ready(Some(packet));
        }
        if self.closed.load(Ordering::Acquire) {
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
    flow: Arc<Flow>,
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
            if let Some(hooks) = hooks {
                hooks.on_io_error(
                    cx,
                    &super::StreamIoError {
                        at: std::time::Instant::now(),
                        direction: super::IoDirection::Send,
                        phase: super::IoPhase::Flush,
                        size: None,
                        error: StreamError::transport(
                            std::io::Error::new(
                                std::io::ErrorKind::BrokenPipe,
                                "control frame discarded before flush",
                            ),
                            super::IoPhase::Flush,
                            true,
                        ),
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
    pub bytes: Bytes,
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
    bytes: Bytes,
    _budget: SendBudget,
}

impl SendCore {
    fn stage<T: Message>(&self, value: &T) -> Result<Staged, StreamError> {
        let mut sizing = pilota::thrift::binary::TBinaryProtocol::new(Bytes::new(), false);
        let size = value.size(&mut sizing);
        self.stage_encoded(size, || encode_message(value))
    }
    fn stage_encoded(
        &self,
        size: usize,
        encode: impl FnOnce() -> Result<Bytes, StreamError>,
    ) -> Result<Staged, StreamError> {
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
        if bytes.len() != size {
            return Err(StreamError::Protocol(
                "encoded size disagrees with reserved length".into(),
            ));
        }
        Ok(Staged {
            bytes,
            _budget: budget,
        })
    }
    async fn ensure_header(&self) -> Result<(), StreamError> {
        if !self.flow.header_sent.load(Ordering::Acquire) {
            let mut frame = self.flow.frame(FrameType::Header, Bytes::new());
            frame.metadata = self.flow.pending_header.lock().clone();
            let bytes = frame.encode(&self.flow.config)?;
            self.tx
                .send(WriteCommand {
                    bytes,
                    flow: Some(self.flow.clone()),
                    finish: None,
                    flushed: None,
                    control_observer: None,
                    reset_id: None,
                    _budget: None,
                    _lease: None,
                })
                .await
                .map_err(|_| StreamError::closed())?;
            self.flow.header_sent.store(true, Ordering::Release);
        }
        Ok(())
    }
    async fn send_staged(
        &mut self,
        staged: Result<Staged, StreamError>,
    ) -> Result<(), StreamError> {
        let staged = staged.inspect_err(|error| {
            self.flow.send_error(error, None);
        })?;
        if let Some(error) = self.flow.cx.cancellation() {
            return Err(error);
        }
        if let Some(status) = self.flow.cx.status() {
            return Err(status
                .error
                .unwrap_or_else(|| StreamError::Protocol("stream is finished".into())));
        }
        if self.flow.send_closed.load(Ordering::Acquire) {
            return Err(StreamError::Protocol("send direction is closed".into()));
        }
        self.ensure_header().await?;
        let bytes =
            self.template
                .encode(self.flow.cx.stream_id(), &staged.bytes, &self.flow.config)?;
        self.tx
            .send(WriteCommand {
                bytes,
                flow: Some(self.flow.clone()),
                finish: None,
                flushed: None,
                control_observer: None,
                reset_id: None,
                _budget: Some(staged._budget),
                _lease: None,
            })
            .await
            .map_err(|_| StreamError::closed())?;
        self.sent(staged.bytes.len());
        Ok(())
    }
    pub async fn send_raw(&mut self, bytes: Bytes) -> Result<(), StreamError> {
        let staged = self.stage_encoded(bytes.len(), || Ok(bytes));
        self.send_staged(staged).await
    }
    fn sent(&self, size: usize) {
        self.flow.sent_count.fetch_add(1, Ordering::Relaxed);
        self.flow
            .sent_total
            .fetch_add(size as u64, Ordering::Relaxed);
        if let Some(hooks) = &self.flow.hooks {
            hooks.on_send(&self.flow.cx, size);
        }
    }
    pub async fn close(&mut self, error: Option<StreamError>) -> Result<(), StreamError> {
        if self.flow.send_closed.load(Ordering::Acquire)
            || self.flow.cx.status().is_some()
            || self.flow.cx.cancellation().is_some()
        {
            return Ok(());
        }
        self.ensure_header().await?;
        let mut frame = self.flow.frame(FrameType::Trailer, Bytes::new());
        frame.metadata = self.flow.pending_trailer.lock().clone();
        if let Some(error) = &error {
            encode_terminal(error, &mut frame)?;
        }
        let bytes = frame.encode(&self.flow.config)?;
        self.tx
            .send(WriteCommand {
                bytes,
                flow: Some(self.flow.clone()),
                finish: (!self.flow.client).then_some(error),
                flushed: None,
                control_observer: None,
                reset_id: None,
                _budget: None,
                _lease: None,
            })
            .await
            .or_else(|_| {
                // A server-streaming response may finish while this half-close waits
                // for the queue. Its successful terminal owns the receive result.
                if self.flow.cx.status().is_some() {
                    Ok(())
                } else {
                    Err(StreamError::closed())
                }
            })?;
        self.flow.send_closed.store(true, Ordering::Release);
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
            Poll::Ready(Some(packet)) => Poll::Ready(Ok(Some(packet.bytes.clone()))),
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
        if let Some(hooks) = &self.flow.hooks {
            hooks.on_recv(&self.flow.cx, size);
        }
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
        if let Some(error) = self.core.flow.cx.cancellation() {
            return Err(error);
        }
        if self.core.flow.send_closed.load(Ordering::Acquire)
            || self.core.flow.cx.status().is_some()
        {
            return Err(StreamError::Protocol("send direction is closed".into()));
        }
        let staged = self.core.stage(value).inspect_err(|error| {
            self.core.flow.send_error(error, None);
        })?;
        let bytes = self.core.template.encode(
            self.core.flow.cx.stream_id(),
            &staged.bytes,
            &self.core.flow.config,
        )?;
        self.permit.send(WriteCommand {
            bytes,
            flow: Some(self.core.flow.clone()),
            finish: None,
            flushed: None,
            control_observer: None,
            reset_id: None,
            _budget: Some(staged._budget),
            _lease: None,
        });
        self.core.sent(staged.bytes.len());
        Ok(())
    }
}

macro_rules! sender {
    ($name:ident) => {
        pub struct $name<T> {
            pub(crate) core: SendCore,
            _type: PhantomData<fn(T)>,
        }
        impl<T> $name<T> {
            pub(crate) fn new(core: SendCore) -> Self {
                Self {
                    core,
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
                if let Some(error) = self.core.flow.cx.cancellation() {
                    return Err(error);
                }
                self.core.ensure_header().await?;
                let permit = self
                    .core
                    .tx
                    .clone()
                    .reserve_owned()
                    .await
                    .map_err(|_| StreamError::closed())?;
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
sender!(RequestSender);
sender!(ResponseSender);

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
impl<T> ResponseSender<T> {
    pub fn set_trailer(&mut self, metadata: Metadata) -> Result<(), StreamError> {
        if self.core.flow.send_closed.load(Ordering::Acquire)
            || self.core.flow.cx.status().is_some()
        {
            return Err(StreamError::Protocol("trailer already committed".into()));
        }
        self.core.flow.pending_trailer.lock().extend(metadata);
        Ok(())
    }
    pub async fn finish(&mut self, result: Result<(), StreamError>) -> Result<(), StreamError> {
        self.core.close(result.err()).await
    }
    pub fn set_header(&mut self, metadata: Metadata) -> Result<(), StreamError> {
        if self.core.flow.header_sent.load(Ordering::Acquire) {
            return Err(StreamError::Protocol("header already sent".into()));
        }
        self.core.flow.pending_header.lock().extend(metadata);
        Ok(())
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
                                this.core.flow.conversion_error(
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

/// Owned raw struct bytes with an already-acquired local staging reservation.
pub struct PreparedMessage {
    staged: Staged,
}

/// Unwrapped Thrift struct bytes. This does not add a Thrift binary length field.
pub struct RawSender {
    core: SendCore,
}
/// Accounts temporary buffers used by a generic converter before Thrift encoding.
/// Dropping this guard returns the same per-stream staging budget.
pub struct ConversionReservation {
    _budget: SendBudget,
}

impl RawSender {
    /// Sends a validated struct payload without a unary message envelope.
    pub async fn send_struct(&mut self, value: super::RawStruct) -> Result<(), StreamError> {
        self.send(value.into_bytes()).await
    }
    pub fn set_header(&mut self, metadata: Metadata) -> Result<(), StreamError> {
        if self.core.flow.header_sent.load(Ordering::Acquire) {
            return Err(StreamError::Protocol("header already sent".into()));
        }
        self.core.flow.pending_header.lock().extend(metadata);
        Ok(())
    }
    pub fn set_trailer(&mut self, metadata: Metadata) -> Result<(), StreamError> {
        if self.core.flow.send_closed.load(Ordering::Acquire)
            || self.core.flow.cx.status().is_some()
        {
            return Err(StreamError::Protocol("trailer already committed".into()));
        }
        self.core.flow.pending_trailer.lock().extend(metadata);
        Ok(())
    }
    pub async fn send_header(&mut self) -> Result<(), StreamError> {
        self.core.ensure_header().await
    }
    pub fn reserve_conversion(&self, bytes: usize) -> Result<ConversionReservation, StreamError> {
        let budget = SendBudget::reserve(self.core.flow.clone(), bytes, "send conversion scratch")
            .inspect_err(|error| self.core.flow.send_error(error, Some(bytes)))?;
        Ok(ConversionReservation { _budget: budget })
    }
    /// Reserves before running a converter. The result owns both bytes and budget,
    /// so the input and converter can be dropped before the first asynchronous poll.
    pub fn prepare_encoded(
        &self,
        encoded_len: usize,
        encode: impl FnOnce() -> Result<Bytes, StreamError>,
    ) -> Result<PreparedMessage, StreamError> {
        self.core
            .stage_encoded(encoded_len, encode)
            .map(|staged| PreparedMessage { staged })
            .inspect_err(|error| {
                self.core.flow.send_error(error, Some(encoded_len));
            })
    }
    pub fn send_prepared<'a>(
        &'a mut self,
        message: PreparedMessage,
    ) -> impl Future<Output = Result<(), StreamError>> + Send + use<'a> {
        let staged = if Arc::ptr_eq(&message.staged._budget.flow, &self.core.flow) {
            Ok(message.staged)
        } else {
            Err(StreamError::configuration(
                "prepared message belongs to another stream",
            ))
        };
        self.core.send_staged(staged)
    }
    /// Reports an outer raw/generic codec failure through the stream's hooks and cancellation.
    pub fn conversion_error(&self, error: StreamError, size: Option<usize>) {
        self.core.flow.conversion_error(
            super::IoDirection::Send,
            super::IoPhase::Encode,
            size,
            error,
        );
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
pub struct ReceiveValidation {
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
    /// Codec adapters that already called `conversion_error` should simply drop
    /// the guard instead, so the I/O event is not reported twice.
    pub fn reject(self, error: StreamError, size: Option<usize>) {
        self.flow.conversion_error(
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
    pub fn begin_receive_validation(&self) -> Result<ReceiveValidation, StreamError> {
        ReceiveValidation::new(self.core.flow.clone())
    }
}

pub struct RawStream {
    core: RecvCore,
}
impl RawStream {
    pub fn begin_receive_validation(&self) -> Result<ReceiveValidation, StreamError> {
        ReceiveValidation::new(self.core.flow.clone())
    }
    pub async fn header(&self) -> Result<Metadata, StreamError> {
        self.core.header().await
    }
    pub fn trailer(&self) -> Option<Metadata> {
        self.core.trailer()
    }
    /// Reports an outer raw/generic codec failure without changing an already observed terminal.
    pub fn conversion_error(&self, error: StreamError, size: Option<usize>) {
        self.core.flow.conversion_error(
            super::IoDirection::Recv,
            super::IoPhase::Decode,
            size,
            error,
        );
    }
    pub fn context(&self) -> &StreamContext {
        &self.core.flow.cx
    }
    /// Receives and validates one struct while retaining Kitex-compatible suffix bytes.
    pub async fn recv_struct(&mut self) -> Result<Option<super::RawStruct>, StreamError> {
        let Some(bytes) = futures::future::poll_fn(|cx| self.core.poll_raw(cx)).await? else {
            return Ok(None);
        };
        let len = bytes.len();
        match super::RawStruct::from_bytes(bytes) {
            Ok(value) => {
                self.core.received(len);
                Ok(Some(value))
            }
            Err(error) => {
                self.core.reported = true;
                self.core.rx.close(true);
                self.conversion_error(error.clone(), Some(len));
                self.core.flow.finish_receive();
                Err(error)
            }
        }
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
    use super::*;
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
            sender.prepare_encoded(5, || {
                called.set(true);
                Ok(Bytes::new())
            }),
            Err(StreamError::ResourceExhausted { .. })
        ));
        assert!(!called.get());
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || sender.prepare_encoded(4, || panic!("codec panic"))
            ))
            .is_err()
        );
        assert_eq!(flow.send_bytes.load(Ordering::Relaxed), 0);
        let prepared = sender
            .prepare_encoded(4, || Ok(Bytes::from_static(b"data")))
            .unwrap();
        assert_eq!(flow.send_bytes.load(Ordering::Relaxed), 4);
        let future = sender.send_prepared(prepared);
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
            sender.prepare_encoded(4, || panic!("must reject before encoding")),
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
            bytes: Bytes::new(),
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
            let bytes = encode_message(&ApplicationException::new(
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
                let bytes = encode_message(&ApplicationException::new(
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

    #[tokio::test]
    async fn raw_validation_defers_final_hook_and_preserves_converter_failure() {
        let (flow, events) = observed_flow(true);
        let mut receiver = RawStream {
            core: RecvCore::new(flow.clone(), flow.inbound.clone()),
        };
        let validation = receiver.begin_receive_validation().unwrap();
        flow.finish(None);
        assert!(receiver.recv().await.unwrap().is_none());
        assert!(events.status.lock().is_empty());
        receiver.conversion_error(
            StreamError::Protocol("generic conversion failed".into()),
            None,
        );
        drop(validation);
        assert_eq!(*events.order.lock(), vec!["io_error", "finish"]);
        assert!(
            matches!(&events.status.lock()[0].error,Some(StreamError::Protocol(message)) if message=="generic conversion failed")
        );
        assert!(receiver.begin_receive_validation().is_err());
        assert_eq!(events.status.lock().len(), 1);
        assert!(flow.cx.status().unwrap().error.is_none());
    }
    #[tokio::test]
    async fn dropped_unfinished_validation_keeps_receiver_usable() {
        let (flow, events) = observed_flow(true);
        let mut receiver = RawStream {
            core: RecvCore::new(flow.clone(), flow.inbound.clone()),
        };
        drop(receiver.begin_receive_validation().unwrap());
        assert!(events.status.lock().is_empty());
        assert!(flow.cx.cancellation().is_none());
        flow.finish(None);
        assert!(events.status.lock().is_empty());
        assert!(receiver.recv().await.unwrap().is_none());
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
