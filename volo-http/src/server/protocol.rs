//! Protocol related types.
//!
//! For more details, refer to [`Http1Config`] and [`Http2Config`].
//!
//! And in most cases, users do not need to pay attention to this mod.

use hyper_util::rt::TokioExecutor;

/// Configuration of the http1 part of the [`Server`].
///
/// This config is created by [`Server::http1_config`]
///
/// [`Server`]: crate::server::Server
/// [`Server::http1_config`]: crate::server::Server::http1_config
#[cfg(feature = "http1")]
pub struct Http1Config<'a> {
    pub(super) inner: hyper_util::server::conn::auto::Http1Builder<'a, TokioExecutor>,
}

#[cfg(feature = "http1")]
impl Http1Config<'_> {
    /// Set whether the `date` header should be included in HTTP responses.
    ///
    /// Note that including the `date` header is recommended by RFC 7231.
    ///
    /// Default is `true`.
    pub fn set_auto_date_header(&mut self, auto_date_header: bool) -> &mut Self {
        self.inner.auto_date_header(auto_date_header);
        self
    }

    /// Set whether HTTP/1 connections should support half-closures.
    ///
    /// Clients can chose to shutdown their write-side while waiting
    /// for the server to respond. Setting this to `true` will
    /// prevent closing the connection immediately if `read`
    /// detects an EOF in the middle of a request.
    ///
    /// Default is `false`.
    pub fn set_half_close(&mut self, half_close: bool) -> &mut Self {
        self.inner.half_close(half_close);
        self
    }

    /// Enables or disables HTTP/1 keep-alive.
    ///
    /// Default is true.
    pub fn set_keep_alive(&mut self, keep_alive: bool) -> &mut Self {
        self.inner.keep_alive(keep_alive);
        self
    }

    /// Set whether HTTP/1 connections will write header names as title case at
    /// the socket level.
    ///
    /// Default is false.
    pub fn set_title_case_headers(&mut self, title_case_headers: bool) -> &mut Self {
        self.inner.title_case_headers(title_case_headers);
        self
    }

    /// Set whether HTTP/1 connections will silently ignored malformed header lines.
    ///
    /// If this is enabled and a header line does not start with a valid header name, or does not
    /// include a colon at all, the line will be silently ignored and no error will be reported.
    ///
    /// Default is `false`.
    pub fn set_ignore_invalid_headers(&mut self, ignore_invalid_headers: bool) -> &mut Self {
        self.inner.ignore_invalid_headers(ignore_invalid_headers);
        self
    }

    /// Set the maximum number of headers.
    ///
    /// When a request is received, the parser will reserve a buffer to store headers for optimal
    /// performance.
    ///
    /// If server receives more headers than the buffer size, it responds to the client with
    /// "431 Request Header Fields Too Large".
    ///
    /// Note that headers is allocated on the stack by default, which has higher performance. After
    /// setting this value, headers will be allocated in heap memory, that is, heap memory
    /// allocation will occur for each request, and there will be a performance drop of about 5%.
    ///
    /// Default is 100.
    pub fn set_max_headers(&mut self, max_headers: usize) -> &mut Self {
        self.inner.max_headers(max_headers);
        self
    }
}

/// Configuration of the http2 part of the [`Server`].
///
/// [`Server`]: crate::server::Server
#[cfg(feature = "http2")]
pub struct Http2Config<'a> {
    pub(super) inner: hyper_util::server::conn::auto::Http2Builder<'a, TokioExecutor>,
}

#[cfg(feature = "http2")]
impl Http2Config<'_> {
    /// Configures the maximum number of pending reset streams allowed before a GOAWAY will be sent.
    ///
    /// This will default to the default value set by the [`h2` crate](https://crates.io/crates/h2).
    /// As of v0.4.0, it is 20.
    ///
    /// See <https://github.com/hyperium/hyper/issues/2877> for more information.
    pub fn max_pending_accept_reset_streams(&mut self, max: impl Into<Option<usize>>) -> &mut Self {
        self.inner.max_pending_accept_reset_streams(max);
        self
    }

    /// Configures the maximum number of local reset streams allowed before a GOAWAY will be sent.
    ///
    /// If not set, hyper will use a default, currently of 1024.
    ///
    /// If `None` is supplied, hyper will not apply any limit.
    /// This is not advised, as it can potentially expose servers to DOS vulnerabilities.
    ///
    /// See <https://rustsec.org/advisories/RUSTSEC-2024-0003.html> for more information.
    pub fn max_local_error_reset_streams(&mut self, max: impl Into<Option<usize>>) -> &mut Self {
        self.inner.max_local_error_reset_streams(max);
        self
    }

    /// Sets the [`SETTINGS_INITIAL_WINDOW_SIZE`][spec] option for HTTP2
    /// stream-level flow control.
    ///
    /// Passing `None` will do nothing.
    ///
    /// If not set, hyper will use a default.
    ///
    /// [spec]: https://http2.github.io/http2-spec/#SETTINGS_INITIAL_WINDOW_SIZE
    pub fn initial_stream_window_size(&mut self, sz: impl Into<Option<u32>>) -> &mut Self {
        self.inner.initial_stream_window_size(sz);
        self
    }

    /// Sets the max connection-level flow control for HTTP2.
    ///
    /// Passing `None` will do nothing.
    ///
    /// If not set, hyper will use a default.
    pub fn initial_connection_window_size(&mut self, sz: impl Into<Option<u32>>) -> &mut Self {
        self.inner.initial_connection_window_size(sz);
        self
    }

    /// Sets whether to use an adaptive flow control.
    ///
    /// Enabling this will override the limits set in
    /// `http2_initial_stream_window_size` and
    /// `http2_initial_connection_window_size`.
    pub fn adaptive_window(&mut self, enabled: bool) -> &mut Self {
        self.inner.adaptive_window(enabled);
        self
    }

    /// Sets the maximum frame size to use for HTTP2.
    ///
    /// Passing `None` will do nothing.
    ///
    /// If not set, hyper will use a default.
    pub fn max_frame_size(&mut self, sz: impl Into<Option<u32>>) -> &mut Self {
        self.inner.max_frame_size(sz);
        self
    }

    /// Sets the [`SETTINGS_MAX_CONCURRENT_STREAMS`][spec] option for HTTP2
    /// connections.
    ///
    /// Default is 200. Passing `None` will remove any limit.
    ///
    /// [spec]: https://http2.github.io/http2-spec/#SETTINGS_MAX_CONCURRENT_STREAMS
    pub fn max_concurrent_streams(&mut self, max: impl Into<Option<u32>>) -> &mut Self {
        self.inner.max_concurrent_streams(max);
        self
    }

    /// Sets an interval for HTTP2 Ping frames should be sent to keep a
    /// connection alive.
    ///
    /// Pass `None` to disable HTTP2 keep-alive.
    ///
    /// Default is currently disabled.
    ///
    /// # Cargo Feature
    pub fn keep_alive_interval(
        &mut self,
        interval: impl Into<Option<std::time::Duration>>,
    ) -> &mut Self {
        self.inner.keep_alive_interval(interval);
        self
    }

    /// Sets a timeout for receiving an acknowledgement of the keep-alive ping.
    ///
    /// If the ping is not acknowledged within the timeout, the connection will
    /// be closed. Does nothing if `http2_keep_alive_interval` is disabled.
    ///
    /// Default is 20 seconds.
    ///
    /// # Cargo Feature
    pub fn keep_alive_timeout(&mut self, timeout: std::time::Duration) -> &mut Self {
        self.inner.keep_alive_timeout(timeout);
        self
    }

    /// Set the maximum write buffer size for each HTTP/2 stream.
    ///
    /// Default is currently ~400KB, but may change.
    ///
    /// # Panics
    ///
    /// The value must be no larger than `u32::MAX`.
    pub fn max_send_buf_size(&mut self, max: usize) -> &mut Self {
        self.inner.max_send_buf_size(max);
        self
    }

    /// Enables the [extended CONNECT protocol].
    ///
    /// [extended CONNECT protocol]: https://datatracker.ietf.org/doc/html/rfc8441#section-4
    pub fn enable_connect_protocol(&mut self) -> &mut Self {
        self.inner.enable_connect_protocol();
        self
    }

    /// Sets the max size of received header frames.
    ///
    /// Default is currently ~16MB, but may change.
    pub fn max_header_list_size(&mut self, max: u32) -> &mut Self {
        self.inner.max_header_list_size(max);
        self
    }

    /// Set whether the `date` header should be included in HTTP responses.
    ///
    /// Note that including the `date` header is recommended by RFC 7231.
    ///
    /// Default is true.
    pub fn auto_date_header(&mut self, enabled: bool) -> &mut Self {
        self.inner.auto_date_header(enabled);
        self
    }
}
