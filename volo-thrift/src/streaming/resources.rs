//! Per-stream local resource observations. This accounts framework-owned buffers,
//! not arbitrary clones retained by application code.
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamResources {
    pub send_buffer_bytes: u64,
    pub send_high_water_bytes: u64,
    pub queued_messages: u64,
    pub queued_bytes: u64,
    pub high_water_messages: u64,
    pub high_water_bytes: u64,
    pub read_pauses: u64,
    pub read_pause_nanos: u64,
    /// Time queued for the per-target opening mutex or pool capacity, including
    /// abandoned waits. Excludes this caller's dial and HEADER enqueue work.
    pub pool_wait_nanos: u64,
    pub control_queue_delay_nanos: u64,
    pub buffer_limit_resets: u64,
    /// Local service/method lookup failures; does not count remote application errors.
    pub route_failures: u64,
    /// Local duplicate active or quarantined stream-ID rejection decisions.
    pub duplicate_sid_rejections: u64,
}
#[derive(Debug, Default)]
pub(crate) struct ResourceCounters {
    pub send_buffer_bytes: AtomicU64,
    pub send_high_water_bytes: AtomicU64,
    pub queued_messages: AtomicU64,
    pub queued_bytes: AtomicU64,
    pub high_water_messages: AtomicU64,
    pub high_water_bytes: AtomicU64,
    pub read_pauses: AtomicU64,
    pub read_pause_nanos: AtomicU64,
    pub pool_wait_nanos: AtomicU64,
    pub control_queue_delay_nanos: AtomicU64,
    pub buffer_limit_resets: AtomicU64,
    pub route_failures: AtomicU64,
    pub duplicate_sid_rejections: AtomicU64,
}
impl ResourceCounters {
    pub fn snapshot(&self) -> StreamResources {
        StreamResources {
            send_buffer_bytes: self.send_buffer_bytes.load(Ordering::Relaxed),
            send_high_water_bytes: self.send_high_water_bytes.load(Ordering::Relaxed),
            queued_messages: self.queued_messages.load(Ordering::Relaxed),
            queued_bytes: self.queued_bytes.load(Ordering::Relaxed),
            high_water_messages: self.high_water_messages.load(Ordering::Relaxed),
            high_water_bytes: self.high_water_bytes.load(Ordering::Relaxed),
            read_pauses: self.read_pauses.load(Ordering::Relaxed),
            read_pause_nanos: self.read_pause_nanos.load(Ordering::Relaxed),
            pool_wait_nanos: self.pool_wait_nanos.load(Ordering::Relaxed),
            control_queue_delay_nanos: self.control_queue_delay_nanos.load(Ordering::Relaxed),
            buffer_limit_resets: self.buffer_limit_resets.load(Ordering::Relaxed),
            route_failures: self.route_failures.load(Ordering::Relaxed),
            duplicate_sid_rejections: self.duplicate_sid_rejections.load(Ordering::Relaxed),
        }
    }
}
pub(crate) fn nanos(duration: std::time::Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}
