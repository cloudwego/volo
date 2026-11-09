use std::{collections::BTreeMap, io, sync::Arc};

use pilota::thrift::ApplicationException;

use super::Metadata;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum IoPhase {
    Open,
    Connect,
    Encode,
    Read,
    Decode,
    Write,
    Flush,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoDirection {
    Send,
    Recv,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CancelCause {
    Local,
    Upstream,
    Downstream,
    Deadline,
    Drop,
    ConnectionClosed,
}

#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StreamError {
    #[error("business status {code}: {message}")]
    Biz {
        code: i32,
        message: String,
        extra: BTreeMap<String, String>,
    },
    #[error(transparent)]
    Application(#[from] ApplicationException),
    #[error("stream canceled ({cause:?}, {code}): {message}")]
    Canceled {
        cause: CancelCause,
        code: i32,
        message: String,
        path: Vec<String>,
    },
    #[error("transport {phase:?} failed (committed={committed}): {source}")]
    Transport {
        source: Arc<io::Error>,
        phase: IoPhase,
        committed: bool,
    },
    #[error("{0}")]
    Protocol(String),
    #[error("TTStream protocol status {code}: {message}")]
    ProtocolStatus { code: i32, message: String },
    #[error("ttstream receive buffer limit exceeded ({scope}): limit={limit}, observed={observed}")]
    ResourceExhausted {
        scope: &'static str,
        limit: usize,
        observed: usize,
    },
    #[error("stream configuration: {0}")]
    Configuration(String),
    #[error("ttstream server draining")]
    ServerDraining,
    #[error("stream receive timeout")]
    RecvTimeout,
}

impl StreamError {
    /// Kitex's public stream exception code; Application retains the payload's
    /// original Thrift kind separately. Business status codes are not exceptions.
    pub fn kitex_code(&self) -> Option<i32> {
        match self {
            Self::Application(_) => Some(12001),
            Self::Protocol(_) => Some(12004),
            Self::ProtocolStatus { code, .. } | Self::Canceled { code, .. } => Some(*code),
            Self::RecvTimeout => Some(12014),
            Self::Transport {
                phase: IoPhase::Connect | IoPhase::Open,
                ..
            }
            | Self::Biz { .. }
            | Self::Configuration(_) => None,
            Self::Transport { .. } => Some(12013),
            Self::ResourceExhausted { .. } | Self::ServerDraining => Some(6),
        }
    }
    pub fn configuration(message: impl Into<String>) -> Self {
        Self::Configuration(message.into())
    }
    pub fn transport(error: io::Error, phase: IoPhase, committed: bool) -> Self {
        Self::Transport {
            source: Arc::new(error),
            phase,
            committed,
        }
    }

    pub(crate) fn closed() -> Self {
        Self::Canceled {
            cause: CancelCause::ConnectionClosed,
            code: 12013,
            message: "ttstream connection closed".into(),
            path: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct StreamIoError {
    pub at: std::time::Instant,
    pub direction: IoDirection,
    pub phase: IoPhase,
    pub size: Option<usize>,
    pub error: StreamError,
}

#[derive(Clone, Debug, Default)]
pub struct StreamStatus {
    pub resources: super::StreamResources,
    pub error: Option<StreamError>,
    pub header: Metadata,
    pub trailer: Metadata,
    pub sent_messages: u64,
    pub sent_bytes: u64,
    pub received_messages: u64,
    pub received_bytes: u64,
}
