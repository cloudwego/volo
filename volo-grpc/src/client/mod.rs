//! gRPC client for Volo.
//!
//! Users should not use this module directly.
//! Instead, they should use the `Builder` type in the generated code.
//!
//! For users need to specify some options at call time, they may use ['callopt'][callopt].

mod callopt;

use std::{marker::PhantomData, sync::Arc, time::Duration};

pub use callopt::CallOpt;
use motore::{
    layer::{Identity, Layer, Stack},
    service::{BoxCloneService, Service},
};
use volo::{
    context::{Endpoint, Role, RpcInfo},
    net::Address,
};

use crate::{
    context::{ClientContext, Config},
    transport::ClientTransport,
    Request, Response, Status,
};

/// Only used by framework generated code.
/// Do not use directly.
#[doc(hidden)]
pub trait SetClient<T, U> {
    fn set_client(self, client: Client<T, U>) -> Self;
}

/// [`ClientBuilder`] provides a [builder-like interface][builder] to construct a [`Client`].
pub struct ClientBuilder<C, L, T, U> {
    http2_config: Http2Config,
    rpc_config: Config,
    callee_name: smol_str::SmolStr,
    caller_name: smol_str::SmolStr,
    // Maybe address use Arc avoid memory alloc.
    target: Option<Address>,
    layer: L,
    service_client: C,
    _marker: PhantomData<fn(T, U)>,
}

impl<C, T, U> ClientBuilder<C, Identity, T, U> {
    /// Creates a new [`ClientBuilder`].
    pub fn new(
        service_client: C,
        service_name: impl AsRef<str>,
    ) -> ClientBuilder<C, Identity, T, U> {
        ClientBuilder {
            http2_config: Default::default(),
            rpc_config: Default::default(),
            callee_name: service_name.into(),
            caller_name: "".into(),
            target: None,
            layer: Identity::new(),
            service_client,
            _marker: PhantomData,
        }
    }
}

impl<C, L, T, U> ClientBuilder<C, L, T, U>
where
    C: SetClient<T, U>,
{
    /// Sets the [`SETTINGS_INITIAL_WINDOW_SIZE`] option for HTTP2
    /// stream-level flow control.
    ///
    /// Default is `2MB`.
    pub fn http2_init_stream_window_size(mut self, sz: impl Into<u32>) -> Self {
        self.http2_config.init_stream_window_size = sz.into();
        self
    }

    /// Sets the max connection-level flow control for HTTP2.
    ///
    /// Default is `5MB`.
    pub fn http2_init_connection_window_size(mut self, sz: impl Into<u32>) -> Self {
        self.http2_config.init_connection_window_size = sz.into();
        self
    }

    /// Sets whether to use an adaptive flow control.
    ///
    /// Enabling this will override the limits set in
    /// `http2_initial_stream_window_size` and
    /// `http2_initial_connection_window_size`.
    ///
    /// Default is `false`.
    pub fn http2_adaptive_window(mut self, enabled: bool) -> Self {
        self.http2_config.adaptive_window = enabled;
        self
    }

    /// Sets the maximum frame size to use for HTTP2.
    ///
    /// Default is `16KB`.
    pub fn http2_max_frame_size(mut self, sz: impl Into<u32>) -> Self {
        self.http2_config.max_frame_size = sz.into();
        self
    }

    /// Sets an interval for HTTP2 Ping frames should be sent to keep a
    /// connection alive.
    ///
    /// Default is disabled.
    pub fn http2_keepalive_interval(mut self, interval: impl Into<Option<Duration>>) -> Self {
        self.http2_config.http2_keepalive_interval = interval.into();
        self
    }

    /// Sets a timeout for receiving an acknowledgement of the keep-alive ping.
    ///
    /// If the ping is not acknowledged within the timeout, the connection will
    /// be closed. Does nothing if `http2_keepalive_interval` is disabled.
    ///
    /// Default is `20` seconds.
    pub fn http2_keepalive_timeout(mut self, timeout: Duration) -> Self {
        self.http2_config.http2_keepalive_timeout = timeout;
        self
    }

    /// Sets whether HTTP2 keep-alive should apply while the connection is idle.
    ///
    /// If disabled, keep-alive pings are only sent while there are open
    /// request/responses streams. If enabled, pings are also sent when no
    /// streams are active. Does nothing if `http2_keepalive_interval` is
    /// disabled.
    ///
    /// Default is `false`.
    pub fn http2_keepalive_while_idle(mut self, enabled: bool) -> Self {
        self.http2_config.http2_keepalive_while_idle = enabled;
        self
    }

    /// Sets the maximum number of HTTP2 concurrent locally reset streams.
    ///
    /// Default is `10`.
    pub fn http2_max_concurrent_reset_streams(mut self, sz: impl Into<usize>) -> Self {
        self.http2_config.max_concurrent_reset_streams = sz.into();
        self
    }

    /// Sets whether to retry requests that get disrupted before ever starting
    /// to write.
    ///
    /// Default is `true`.
    pub fn retry_canceled_requests(mut self, enabled: bool) -> Self {
        self.http2_config.retry_canceled_requests = enabled;
        self
    }

    /// Sets whether the connection **must** use HTTP/2.
    ///
    /// Default is `false`.
    pub fn accept_http1(mut self, accept_http1: bool) -> Self {
        self.http2_config.accept_http1 = accept_http1;
        self
    }

    /// Sets that all sockets have `SO_KEEPALIVE` set with the supplied duration.
    ///
    /// If `None`, the option will not be set.
    ///
    /// Default is `None`.
    pub fn tcp_keepalive(mut self, dur: impl Into<Option<Duration>>) -> Self {
        self.http2_config.tcp_keepalive = dur.into();
        self
    }

    /// Sets that all sockets have `SO_NODELAY` set to the supplied value `nodelay`.
    ///
    /// Default is `true`.
    pub fn tcp_nodelay(mut self, nodelay: bool) -> Self {
        self.http2_config.tcp_nodelay = nodelay;
        self
    }

    /// Sets the timeout for connecting to a URL.
    ///
    /// Default is no timeout.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.rpc_config.connect_timeout = Some(timeout);
        self
    }

    /// Sets the timeout for the response.
    ///
    /// Default is no timeout.
    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.rpc_config.read_timeout = Some(timeout);
        self
    }

    /// Sets the timeout for the request.
    ///
    /// Default is no timeout.
    pub fn write_timeout(mut self, timeout: Duration) -> Self {
        self.rpc_config.write_timeout = Some(timeout);
        self
    }

    /// Sets the caller name for the client.
    ///
    /// Default is the empty string.
    pub fn caller_name(mut self, name: impl AsRef<str>) -> Self {
        self.caller_name = name.into();
        self
    }

    /// Sets the address for the rpc call.
    ///
    /// Default is None.
    pub fn target<A: Into<Address>>(mut self, target: A) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Adds a new layer to the client.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer(baz)`, we will get: foo -> bar -> baz.
    pub fn layer<O>(self, layer: O) -> ClientBuilder<C, Stack<O, L>, T, U> {
        ClientBuilder {
            http2_config: self.http2_config,
            rpc_config: self.rpc_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            layer: Stack::new(layer, self.layer),
            service_client: self.service_client,
            _marker: self._marker,
        }
    }
}

impl<T, U, C, L> ClientBuilder<C, L, T, U>
where
    C: SetClient<T, U>,
    T: 'static,
{
    /// Builds a new [`Client`].
    pub fn build(self) -> C
    where
        L: Layer<ClientTransport<U>>,
        L::Service: Service<ClientContext, Request<T>, Response = Response<U>, Error = Status>
            + Clone
            + Send
            + 'static,
    {
        let transport = ClientTransport::new(&self.http2_config, &self.rpc_config);
        let transport = self.layer.layer(transport);
        let transport = BoxCloneService::new(transport);

        self.service_client.set_client(Client {
            inner: Arc::new(ClientInner {
                callee_name: self.callee_name,
                caller_name: self.caller_name,
                rpc_config: self.rpc_config,
                target: self.target,
            }),
            callopt: None,
            transport,
        })
    }
}

/// A struct indicating the rpc configuration of the client.
struct ClientInner {
    callee_name: smol_str::SmolStr,
    caller_name: smol_str::SmolStr,
    rpc_config: Config,
    target: Option<Address>,
}

/// A client for a gRPC service.
///
/// `Client` is designed to "clone and use", so it's cheap to clone it.
/// One important thing is that the `CallOpt` will not be cloned, because
/// it's designed to be per-request.
pub struct Client<T, U> {
    inner: Arc<ClientInner>,
    callopt: Option<CallOpt>,
    transport: BoxCloneService<ClientContext, Request<T>, Response<U>, Status>,
}

/// # Safety
///
/// `Client` doesn't have non-atomic interior mutability.
unsafe impl<T, U> Sync for Client<T, U> {}

impl<T, U> Clone for Client<T, U> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            callopt: None,
            transport: self.transport.clone(),
        }
    }
}

impl<T, U> Client<T, U> {
    pub async fn call(
        &mut self,
        path: &'static str,
        req: Request<T>,
    ) -> Result<Response<U>, Status> {
        let mut cx = ClientContext::new(self.make_rpc_info(path));
        self.transport.call(&mut cx, req).await
    }

    #[inline]
    pub fn set_callopt(&mut self, callopt: CallOpt) {
        self.callopt = Some(callopt);
    }

    fn make_rpc_info(&mut self, method: &'static str) -> RpcInfo<Config> {
        let mut caller = Endpoint::new(self.inner.caller_name.clone());
        let mut callee = Endpoint::new(self.inner.callee_name.clone());
        if let Some(target) = &self.inner.target {
            callee.set_address(target.clone());
        }
        let mut rpc_config = self.inner.rpc_config;
        if let Some(co) = self.callopt.take() {
            caller.tags.extend(co.caller_tags);
            callee.tags.extend(co.callee_tags);
            if let Some(addr) = co.address {
                callee.set_address(addr);
            }
            rpc_config.merge(co.config);
        }
        RpcInfo::new(Role::Client, method.into(), caller, callee, rpc_config)
    }
}

const DEFAULT_STREAM_WINDOW_SIZE: u32 = 1024 * 1024 * 2; // 2MB
const DEFAULT_CONN_WINDOW_SIZE: u32 = 1024 * 1024 * 5; // 5MB
const DEFAULT_MAX_FRAME_SIZE: u32 = 1024 * 16; // 16KB
const DEFAULT_KEEPALIVE_TIMEOUT_SECS: Duration = Duration::from_secs(20); // 20s
const DEFAULT_MAX_CONCURRENT_RESET_STREAMS: usize = 10;

/// Configuration for the underlying h2 connection.
#[derive(Debug, Clone, Copy)]
pub struct Http2Config {
    pub(crate) init_stream_window_size: u32,
    pub(crate) init_connection_window_size: u32,
    pub(crate) adaptive_window: bool,
    pub(crate) max_frame_size: u32,
    pub(crate) http2_keepalive_interval: Option<Duration>,
    pub(crate) http2_keepalive_timeout: Duration,
    pub(crate) http2_keepalive_while_idle: bool,
    pub(crate) max_concurrent_reset_streams: usize,
    pub(crate) retry_canceled_requests: bool,
    pub(crate) accept_http1: bool,
    pub(crate) tcp_keepalive: Option<Duration>,
    pub(crate) tcp_nodelay: bool,
}

impl Default for Http2Config {
    fn default() -> Self {
        Self {
            init_stream_window_size: DEFAULT_STREAM_WINDOW_SIZE,
            init_connection_window_size: DEFAULT_CONN_WINDOW_SIZE,
            adaptive_window: false,
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
            http2_keepalive_interval: None,
            http2_keepalive_timeout: DEFAULT_KEEPALIVE_TIMEOUT_SECS,
            http2_keepalive_while_idle: false,
            max_concurrent_reset_streams: DEFAULT_MAX_CONCURRENT_RESET_STREAMS,
            retry_canceled_requests: true,
            accept_http1: false,
            tcp_keepalive: None,
            tcp_nodelay: true,
        }
    }
}
