use std::{collections::BTreeMap, io, sync::Arc};

use bytes::Bytes;

use super::Metadata;

pub type RpcResult<T> = Result<T, RpcError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CancelCause {
    Local,
    Upstream,
    Downstream,
    Deadline,
    Drop,
    ConnectionClosed,
    ServerShutdown,
}

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

#[derive(Clone, Debug)]
pub struct GrpcStatus {
    pub code: i32,
    pub message: String,
    pub details: Bytes,
    pub metadata: Metadata,
    pub raw_code: Option<Bytes>,
    pub source: Option<Arc<dyn std::error::Error + Send + Sync>>,
}

/// A call error with its native status information retained at the boundary.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum RpcError {
    Biz {
        code: i32,
        message: String,
        extra: BTreeMap<String, String>,
    },
    Application {
        kind: i32,
        message: String,
    },
    Canceled {
        cause: CancelCause,
        code: i32,
        message: String,
        path: Vec<String>,
    },
    Transport {
        source: Arc<io::Error>,
        phase: IoPhase,
        committed: bool,
    },
    Protocol(String),
    ProtocolStatus {
        code: i32,
        message: String,
    },
    ResourceExhausted {
        scope: &'static str,
        limit: usize,
        observed: usize,
    },
    GrpcStatus(Box<GrpcStatus>),
    Configuration(String),
    ServerDraining,
    RecvTimeout,
}

impl RpcError {
    pub fn canceled(message: impl Into<String>) -> Self {
        Self::Canceled {
            cause: CancelCause::Local,
            code: 0,
            message: message.into(),
            path: Vec::new(),
        }
    }
}

impl From<super::RegistrationError> for RpcError {
    fn from(error: super::RegistrationError) -> Self {
        Self::Configuration(error.to_string())
    }
}

impl std::fmt::Display for GrpcStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "gRPC status {}: {}", self.code, self.message)
    }
}
impl std::error::Error for GrpcStatus {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|error| error as &(dyn std::error::Error + 'static))
    }
}
impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Biz { code, message, .. } => write!(f, "business status {code}: {message}"),
            Self::Application { kind, message } => {
                write!(f, "application exception {kind}: {message}")
            }
            Self::Canceled {
                cause,
                code,
                message,
                ..
            } => write!(f, "RPC canceled ({cause:?}, {code}): {message}"),
            Self::Transport {
                source,
                phase,
                committed,
            } => write!(
                f,
                "transport {phase:?} failed (committed={committed}): {source}"
            ),
            Self::Protocol(message) => write!(f, "protocol error: {message}"),
            Self::ProtocolStatus { code, message } => {
                write!(f, "protocol status {code}: {message}")
            }
            Self::ResourceExhausted {
                scope,
                limit,
                observed,
            } => write!(
                f,
                "receive buffer limit exceeded ({scope}): limit={limit}, observed={observed}"
            ),
            Self::GrpcStatus(status) => std::fmt::Display::fmt(status, f),
            Self::Configuration(message) => write!(f, "stream configuration: {message}"),
            Self::ServerDraining => f.write_str("server draining"),
            Self::RecvTimeout => f.write_str("stream receive timeout"),
        }
    }
}
impl std::error::Error for RpcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport { source, .. } => Some(source.as_ref()),
            Self::GrpcStatus(status) => std::error::Error::source(status.as_ref()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;
    #[test]
    fn shared_sources_expose_the_original_concrete_error() {
        let error = RpcError::Transport {
            source: Arc::new(io::Error::other("fixture")),
            phase: IoPhase::Read,
            committed: true,
        };
        assert!(
            error
                .source()
                .unwrap()
                .downcast_ref::<io::Error>()
                .is_some()
        );
        let status = GrpcStatus {
            code: 13,
            message: "failure".into(),
            details: Bytes::new(),
            metadata: Metadata::default(),
            raw_code: None,
            source: Some(Arc::new(io::Error::other("fixture"))),
        };
        assert!(
            status
                .source()
                .unwrap()
                .downcast_ref::<io::Error>()
                .is_some()
        );
    }
}
