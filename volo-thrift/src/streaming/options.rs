use std::time::Duration;

use super::StreamError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum StreamingProtocol {
    #[default]
    TtHeader,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PoolMode {
    Short,
    #[default]
    Long,
    Multiplex,
}

/// Local safety budgets; TTStream has no wire flow-control window.
#[derive(Clone, Debug)]
pub struct StreamConfig {
    pub max_frame_bytes: usize,
    pub max_header_bytes: usize,
    pub max_metadata_entries: usize,
    pub recv_messages: usize,
    pub recv_bytes: usize,
    pub connection_recv_bytes: usize,
    pub shared_recv_bytes: usize,
    pub send_staging_bytes: usize,
    pub connection_send_bytes: usize,
    pub shared_send_bytes: usize,
    pub write_queue_frames: usize,
    pub control_queue_frames: usize,
    pub batch_frames: usize,
    pub batch_bytes: usize,
    pub batch_delay: Duration,
    pub max_read_pause: Duration,
    pub max_streams: usize,
    pub max_quarantined_ids: usize,
    pub pool_mode: PoolMode,
    pub pool_idle_timeout: Duration,
    /// When retiring just after local RST, allow the peer terminal or this bound
    /// before closing; Go netpoll can otherwise observe EOF before its RST.
    pub reset_retire_timeout: Duration,
    pub pool_max_active: usize,
    pub pool_max_idle: usize,
    pub pool_wait_timeout: Duration,
    pub deadline: Option<Duration>,
    pub drain_timeout: Duration,
    pub shutdown_flush_timeout: Duration,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            max_frame_bytes: 16 * 1024 * 1024,
            max_header_bytes: 65535 * 4,
            max_metadata_entries: 16384,
            recv_messages: 4096,
            recv_bytes: 64 * 1024 * 1024,
            connection_recv_bytes: 256 * 1024 * 1024,
            shared_recv_bytes: 1024 * 1024 * 1024,
            send_staging_bytes: 64 * 1024 * 1024,
            connection_send_bytes: 256 * 1024 * 1024,
            shared_send_bytes: 1024 * 1024 * 1024,
            write_queue_frames: 4096,
            control_queue_frames: 256,
            batch_frames: 64,
            batch_bytes: 256 * 1024,
            batch_delay: Duration::from_millis(1),
            max_read_pause: Duration::ZERO,
            max_streams: 4096,
            max_quarantined_ids: 4096,
            pool_mode: PoolMode::Long,
            pool_idle_timeout: Duration::from_secs(60),
            reset_retire_timeout: Duration::from_millis(100),
            pool_max_active: 1024,
            pool_max_idle: 64,
            pool_wait_timeout: Duration::from_secs(5),
            deadline: None,
            drain_timeout: Duration::from_secs(30),
            shutdown_flush_timeout: Duration::from_millis(100),
        }
    }
}

impl StreamConfig {
    pub fn validate(&self) -> Result<(), StreamError> {
        if self.max_frame_bytes < 18
            || self.max_frame_bytes > u32::MAX as usize
            || self.max_header_bytes < 4
            || self.max_header_bytes > 65535 * 4
            || self.recv_messages == 0
            || self.recv_bytes == 0
            || self.max_streams == 0
            || self.write_queue_frames == 0
            || self.control_queue_frames == 0
            || self.batch_frames == 0
            || self.batch_bytes == 0
            || self.pool_max_active == 0
        {
            return Err(StreamError::Configuration(
                "invalid zero or out-of-range TTStream budget".into(),
            ));
        }
        Ok(())
    }
}

/// Generated method shape, for adapters and observation only; never sent on the wire.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum StreamMode {
    Client = 1,
    Server = 2,
    Bidirectional = 3,
    #[default]
    Raw = 0,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecvTimeoutPolicy {
    #[default]
    KeepOpen,
    Cancel,
}
