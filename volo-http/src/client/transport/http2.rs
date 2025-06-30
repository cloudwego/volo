//! HTTP/2 related utilities

use std::time::Duration;

use hyper::client::conn::http2::Builder;
use hyper_util::rt::TokioExecutor;

/// Configurations of HTTP1 Client.
pub struct Config {
    keep_alive_interval: Option<Duration>,
    keep_alive_timeout: Duration,
    keep_alive_while_idle: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            keep_alive_interval: None,
            keep_alive_timeout: Duration::from_secs(20),
            keep_alive_while_idle: false,
        }
    }
}

impl Config {
    /// Sets an interval for HTTP2 Ping frames should be sent to keep a
    /// connection alive.
    ///
    /// Pass `None` to disable HTTP2 keep-alive.
    ///
    /// Default is currently disabled.
    pub fn set_keep_alive_interval<D>(&mut self, interval: D) -> &mut Self
    where
        D: Into<Option<Duration>>,
    {
        self.keep_alive_interval = interval.into();
        self
    }

    /// Sets a timeout for receiving an acknowledgement of the keep-alive ping.
    ///
    /// If the ping is not acknowledged within the timeout, the connection will
    /// be closed. Does nothing if `keep_alive_interval` is disabled.
    ///
    /// Default is 20 seconds.
    pub fn set_keep_alive_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.keep_alive_timeout = timeout;
        self
    }

    /// Sets whether HTTP2 keep-alive should apply while the connection is idle.
    ///
    /// If disabled, keep-alive pings are only sent while there are open
    /// request/responses streams. If enabled, pings are also sent when no
    /// streams are active. Does nothing if `keep_alive_interval` is
    /// disabled.
    ///
    /// Default is `false`.
    pub fn set_keep_alive_while_idle(&mut self, enabled: bool) -> &mut Self {
        self.keep_alive_while_idle = enabled;
        self
    }
}

pub(crate) fn client(config: &Config) -> Builder<TokioExecutor> {
    let mut builder = Builder::new(TokioExecutor::new());
    builder
        .keep_alive_interval(config.keep_alive_interval)
        .keep_alive_timeout(config.keep_alive_timeout)
        .keep_alive_while_idle(config.keep_alive_while_idle);
    builder
}
