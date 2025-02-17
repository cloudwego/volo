//! This module is used internally.

pub const ENABLE_REMOTE_CLOSED_ERROR_LOG_ENV_KEY: &str = "VOLO_ENABLE_REMOTE_CLOSED_ERROR_LOG";

pub static ENABLE_REMOTE_CLOSE_ERROR_LOG: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var(ENABLE_REMOTE_CLOSED_ERROR_LOG_ENV_KEY).is_ok());

pub fn remote_closed_error_log_enabled() -> bool {
    *ENABLE_REMOTE_CLOSE_ERROR_LOG
}

pub fn is_remote_closed_error(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::ConnectionReset
        || err.kind() == std::io::ErrorKind::ConnectionAborted
        || err.kind() == std::io::ErrorKind::BrokenPipe
}
