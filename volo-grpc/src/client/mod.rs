//! gRPC client for Volo.
//!
//! Users should not use this module directly.
//! Instead, they should use the `Builder` type in the generated code.
//!
//! For users need to specify some options at call time, they may use [`CallOpt`].

mod callopt;
pub mod dns;
mod meta;

use std::{cell::RefCell, marker::PhantomData, sync::Arc, time::Duration};

pub use callopt::CallOpt;
pub use meta::MetaService;
use motore::{
    layer::{Identity, Layer, Stack},
    service::{BoxCloneService, Service},
    ServiceExt,
};
use volo::{
    client::{MkClient, WithOptService},
    context::{Endpoint, Role, RpcInfo},
    discovery::Discover,
    loadbalance::{random::WeightedRandomBalance, MkLbLayer},
    net::Address,
    FastStr,
};

use self::{dns::DnsResolver, layer::timeout::TimeoutLayer};
use crate::{
    codec::compression::CompressionEncoding,
    context::{ClientContext, Config},
    layer::loadbalance::LbConfig,
    transport::ClientTransport,
    Request, Response, Status,
};
pub mod layer;

/// [`ClientBuilder`] provides a builder-like interface to construct a [`Client`].
pub struct ClientBuilder<IL, OL, C, LB, T, U> {
    http2_config: Http2Config,
    rpc_config: Config,
    callee_name: FastStr,
    caller_name: FastStr,
    // Maybe address use Arc avoid memory alloc.
    target: Option<Address>,
    inner_layer: IL,
    outer_layer: OL,
    mk_client: C,
    mk_lb: LB,
    _marker: PhantomData<fn(T, U)>,

    #[cfg(feature = "__tls")]
    tls_config: Option<volo::net::tls::ClientTlsConfig>,
}

impl<C, T, U>
    ClientBuilder<
        Identity,
        Identity,
        C,
        LbConfig<WeightedRandomBalance<<DnsResolver as Discover>::Key>, DnsResolver>,
        T,
        U,
    >
{
    /// Creates a new [`ClientBuilder`].
    pub fn new(service_client: C, service_name: impl AsRef<str>) -> Self {
        Self {
            http2_config: Default::default(),
            rpc_config: Default::default(),
            callee_name: FastStr::new(service_name),
            caller_name: "".into(),
            target: None,
            inner_layer: Identity::new(),
            outer_layer: Identity::new(),
            mk_client: service_client,
            mk_lb: LbConfig::new(WeightedRandomBalance::new(), DnsResolver::default()),
            _marker: PhantomData,

            #[cfg(feature = "__tls")]
            tls_config: None,
        }
    }
}

impl<IL, OL, C, LB, T, U, DISC> ClientBuilder<IL, OL, C, LbConfig<LB, DISC>, T, U> {
    pub fn load_balance<NLB>(
        self,
        load_balance: NLB,
    ) -> ClientBuilder<IL, OL, C, LbConfig<NLB, DISC>, T, U> {
        ClientBuilder {
            http2_config: self.http2_config,
            rpc_config: self.rpc_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            mk_lb: self.mk_lb.load_balance(load_balance),
            _marker: PhantomData,

            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    pub fn discover<NDISC>(
        self,
        discover: NDISC,
    ) -> ClientBuilder<IL, OL, C, LbConfig<LB, NDISC>, T, U> {
        ClientBuilder {
            http2_config: self.http2_config,
            rpc_config: self.rpc_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            mk_lb: self.mk_lb.discover(discover),
            _marker: PhantomData,

            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }
}

impl<IL, OL, C, LB, T, U> ClientBuilder<IL, OL, C, LB, T, U> {
    /// Sets the rpc timeout for the client.
    ///
    /// The default value is 1 second.
    ///
    /// Users can set this to `None` to disable the timeout.
    pub fn rpc_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.rpc_config.set_rpc_timeout(timeout);
        self
    }
    /// Sets the `SETTINGS_INITIAL_WINDOW_SIZE` option for HTTP2
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

    /// Set the maximum write buffer size for each HTTP/2 stream.
    ///
    /// Default is currently 1MB, but may change.
    ///
    /// The value must be no larger than `u32::MAX`.
    pub fn http2_max_send_buf_size(mut self, max: impl Into<usize>) -> Self {
        self.http2_config.max_send_buf_size = max.into();
        self
    }

    /// Sets whether to retry requests that get disrupted before ever starting
    /// to write.
    ///
    /// Default is `true`.
    #[deprecated(
        since = "0.9.0",
        note = "`retry_canceled_requests` has been removed in `hyper`"
    )]
    pub fn retry_canceled_requests(self, _enabled: bool) -> Self {
        self
    }

    /// Sets whether the connection **must** use HTTP/2.
    ///
    /// Default is `false`.
    #[deprecated(
        since = "0.9.0",
        note = "accepting http1 connection was not supported by `hyper`"
    )]
    pub fn accept_http1(self, _accept_http1: bool) -> Self {
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
        self.caller_name = FastStr::new(name);
        self
    }

    /// Sets the send compression encodings for the request, and will self-adaptive with config of
    /// the server.
    ///
    /// Default is disable the send compression.
    pub fn send_compressions(mut self, config: Vec<CompressionEncoding>) -> Self {
        self.rpc_config.send_compressions = Some(config);
        self
    }

    /// Sets the accept compression encodings for the request, and will self-adaptive with config of
    /// the server.
    ///
    /// Default is disable the accept decompression.
    pub fn accept_compressions(mut self, config: Vec<CompressionEncoding>) -> Self {
        self.rpc_config.accept_compressions = Some(config);
        self
    }

    pub fn mk_load_balance<NLB>(self, mk_load_balance: NLB) -> ClientBuilder<IL, OL, C, NLB, T, U> {
        ClientBuilder {
            http2_config: self.http2_config,
            rpc_config: self.rpc_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            mk_lb: mk_load_balance,
            _marker: PhantomData,

            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Sets the address for the rpc call.
    ///
    /// If the address is set, the call will be sent to the address directly.
    ///
    /// The client will skip the discovery and loadbalance Service if this is set.
    pub fn address<A: Into<Address>>(mut self, target: A) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Adds a new inner layer to the client.
    ///
    /// The layer's `Service` should be `Send + Sync + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer_inner(baz)`, we will get: foo -> bar -> baz.
    ///
    /// The overall order for layers is: outer -> LoadBalance -> \[inner\] -> transport.
    pub fn layer_inner<Inner>(
        self,
        layer: Inner,
    ) -> ClientBuilder<Stack<Inner, IL>, OL, C, LB, T, U> {
        ClientBuilder {
            http2_config: self.http2_config,
            rpc_config: self.rpc_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            inner_layer: Stack::new(layer, self.inner_layer),
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            mk_lb: self.mk_lb,
            _marker: self._marker,

            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Adds a new inner layer to the client.
    ///
    /// The layer's `Service` should be `Send + Sync + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer_inner_front(baz)`, we will get: baz -> foo -> bar.
    ///
    /// The overall order for layers is: outer -> LoadBalance -> \[inner\] -> transport.
    pub fn layer_inner_front<Inner>(
        self,
        layer: Inner,
    ) -> ClientBuilder<Stack<IL, Inner>, OL, C, LB, T, U> {
        ClientBuilder {
            http2_config: self.http2_config,
            rpc_config: self.rpc_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            inner_layer: Stack::new(self.inner_layer, layer),
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            mk_lb: self.mk_lb,
            _marker: self._marker,

            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Adds a new outer layer to the client.
    ///
    /// The layer's `Service` should be `Send + Sync + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer_outer(baz)`, we will get: foo -> bar -> baz.
    ///
    /// The overall order for layers is: \[outer\] -> LoadBalance -> inner -> transport.
    pub fn layer_outer<Outer>(
        self,
        layer: Outer,
    ) -> ClientBuilder<IL, Stack<Outer, OL>, C, LB, T, U> {
        ClientBuilder {
            http2_config: self.http2_config,
            rpc_config: self.rpc_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            inner_layer: self.inner_layer,
            outer_layer: Stack::new(layer, self.outer_layer),
            mk_client: self.mk_client,
            mk_lb: self.mk_lb,
            _marker: self._marker,

            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Adds a new outer layer to the client.
    ///
    /// The layer's `Service` should be `Send + Sync + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer_outer_front(baz)`, we will get: baz -> foo -> bar.
    ///
    /// The overall order for layers is: \[outer\] -> LoadBalance -> inner -> transport.
    pub fn layer_outer_front<Outer>(
        self,
        layer: Outer,
    ) -> ClientBuilder<IL, Stack<OL, Outer>, C, LB, T, U> {
        ClientBuilder {
            http2_config: self.http2_config,
            rpc_config: self.rpc_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            inner_layer: self.inner_layer,
            outer_layer: Stack::new(self.outer_layer, layer),
            mk_client: self.mk_client,
            mk_lb: self.mk_lb,
            _marker: self._marker,

            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Sets the [`ClientTlsConfig`][ClientTlsConfig] for the client.
    ///
    /// [ClientTlsConfig]: volo::net::tls::ClientTlsConfig
    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    pub fn tls_config(mut self, tls_config: volo::net::tls::ClientTlsConfig) -> Self {
        self.tls_config = Some(tls_config);
        self
    }
}

impl<IL, OL, C, LB, T, U> ClientBuilder<IL, OL, C, LB, T, U>
where
    C: MkClient<Client<BoxCloneService<ClientContext, Request<T>, Response<U>, Status>>>,
    LB: MkLbLayer,
    LB::Layer: Layer<IL::Service>,
    <LB::Layer as Layer<IL::Service>>::Service:
        Service<ClientContext, Request<T>, Response = Response<U>> + 'static + Send + Clone + Sync,
    <<LB::Layer as Layer<IL::Service>>::Service as Service<ClientContext, Request<T>>>::Error:
        Into<Status>,
    IL: Layer<MetaService<ClientTransport<U>>>,
    IL::Service:
        Service<ClientContext, Request<T>, Response = Response<U>> + 'static + Send + Clone + Sync,
    <IL::Service as Service<ClientContext, Request<T>>>::Error: Into<Status>,
    OL:
        Layer<
            BoxCloneService<
                ClientContext,
                Request<T>,
                Response<U>,
                <<LB::Layer as Layer<IL::Service>>::Service as Service<
                    ClientContext,
                    Request<T>,
                >>::Error,
            >,
        >,
    OL::Service:
        Service<ClientContext, Request<T>, Response = Response<U>> + 'static + Send + Clone + Sync,
    <OL::Service as Service<ClientContext, Request<T>>>::Error: Send + Into<Status>,
    T: 'static + Send,
{
    /// Builds a new [`Client`].
    pub fn build(self) -> C::Target {
        #[cfg(not(feature = "__tls"))]
        let transport =
            MetaService::new(ClientTransport::new(&self.http2_config, &self.rpc_config));
        #[cfg(feature = "__tls")]
        let transport = match self.tls_config {
            Some(tls_config) => MetaService::new(ClientTransport::new_with_tls(
                &self.http2_config,
                &self.rpc_config,
                tls_config,
            )),
            None => MetaService::new(ClientTransport::new(&self.http2_config, &self.rpc_config)),
        };

        let transport = self.outer_layer.layer(BoxCloneService::new(
            self.mk_lb.make().layer(self.inner_layer.layer(transport)),
        ));

        let transport = transport.map_err(|err| err.into());
        let transport = TimeoutLayer::new().layer(transport);
        let transport = BoxCloneService::new(transport);

        self.mk_client.mk_client(Client {
            inner: Arc::new(ClientInner {
                callee_name: self.callee_name,
                caller_name: self.caller_name,
                rpc_config: self.rpc_config,
                target: self.target,
            }),
            transport,
        })
    }
}

#[derive(Debug)]
/// A struct indicating the rpc configuration of the client.
struct ClientInner {
    callee_name: FastStr,
    caller_name: FastStr,
    rpc_config: Config,
    target: Option<Address>,
}

/// A client for a gRPC service.
///
/// `Client` is designed to "clone and use", so it's cheap to clone it.
/// One important thing is that the `CallOpt` will not be cloned, because
/// it's designed to be per-request.
#[derive(Clone)]
pub struct Client<S> {
    transport: S,
    inner: Arc<ClientInner>,
}

impl<S> Client<S> {
    pub fn make_cx(&self, path: &'static str) -> ClientContext {
        ClientContext::new(self.make_rpc_info(path))
    }

    fn make_rpc_info(&self, method: &'static str) -> RpcInfo<Config> {
        let caller = Endpoint::new(self.inner.caller_name.clone());
        let mut callee = Endpoint::new(self.inner.callee_name.clone());
        if let Some(target) = &self.inner.target {
            callee.set_address(target.clone());
        }
        RpcInfo::new(
            Role::Client,
            method.into(),
            caller,
            callee,
            self.inner.rpc_config.clone(),
        )
    }

    pub fn with_opt<Opt>(self, opt: Opt) -> Client<WithOptService<S, Opt>> {
        Client {
            transport: WithOptService::new(self.transport, opt),
            inner: self.inner,
        }
    }
}

macro_rules! impl_client {
    (($self: ident, &mut $cx:ident, $req: ident) => async move $e: tt ) => {
        impl<S, Req: Send + 'static>
            volo::service::Service<crate::context::ClientContext, Req> for Client<S>
        where
            S: volo::service::Service<
                    crate::context::ClientContext,
                    Req,
                    Error = crate::Status,
                > + Sync
                + Send
                + 'static,
        {
            type Response = S::Response;
            type Error = S::Error;

            async fn call(
                &$self,
                $cx: &mut crate::context::ClientContext,
                $req: Req,
            ) -> Result<Self::Response, Self::Error> {
                $e
            }
        }

        impl<S, Req: Send + 'static>
            volo::client::OneShotService<crate::context::ClientContext, Req> for Client<S>
        where
            S: volo::client::OneShotService<
                    crate::context::ClientContext,
                    Req,
                    Error = crate::Status,
                > + Sync
                + Send
                + 'static,
        {
            type Response = S::Response;
            type Error = S::Error;

            async fn call(
                $self,
                $cx: &mut crate::context::ClientContext,
                $req: Req,
            ) -> Result<Self::Response, Self::Error> {
                $e
            }
        }
    };
}

impl_client!((self, &mut cx, req) => async move {
    let has_metainfo = metainfo::METAINFO.try_with(|_| {}).is_ok();

    let mk_call = async { self.transport.call(cx, req).await };

    if has_metainfo {
        mk_call.await
    } else {
        metainfo::METAINFO
            .scope(RefCell::new(metainfo::MetaInfo::default()), mk_call)
            .await
    }
});

const DEFAULT_STREAM_WINDOW_SIZE: u32 = 1024 * 1024 * 2; // 2MB
const DEFAULT_CONN_WINDOW_SIZE: u32 = 1024 * 1024 * 5; // 5MB
const DEFAULT_MAX_FRAME_SIZE: u32 = 1024 * 16; // 16KB
const DEFAULT_MAX_SEND_BUF_SIZE: usize = 1024 * 1024; // 1MB
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
    pub(crate) max_send_buf_size: usize,
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
            max_send_buf_size: DEFAULT_MAX_SEND_BUF_SIZE,
        }
    }
}
