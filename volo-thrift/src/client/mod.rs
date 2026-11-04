//! Thrift client for Volo.
//!
//! Users should not use this module directly.
//! Instead, they should use the `Builder` type in the generated code.
//!
//! For users need to specify some options at call time, they may use [`CallOpt`].

use std::{
    cell::RefCell,
    marker::PhantomData,
    sync::{Arc, atomic::AtomicI32},
};

use motore::{
    layer::{Identity, Layer, Stack},
    service::{BoxCloneService, Service},
};
use pilota::thrift::TMessageType;
use tokio::time::Duration;
use volo::{
    FastStr,
    client::WithOptService,
    context::{Context, Endpoint, Role, RpcInfo},
    discovery::{Discover, DummyDiscover},
    loadbalance::{LbConfig, MkLbLayer, random::WeightedRandomBalance},
    net::{
        Address,
        dial::{DefaultMakeTransport, MakeTransport},
    },
};

use crate::{
    ClientError, EntryMessage, ThriftMessage,
    codec::{
        DefaultMakeCodec, MakeCodec,
        default::{framed::MakeFramedCodec, thrift::MakeThriftCodec, ttheader::MakeTTHeaderCodec},
    },
    context::{CLIENT_CONTEXT_CACHE, ClientContext, Config},
    transport::{pingpong, pool},
};

mod callopt;
pub use callopt::CallOpt;

use self::layer::timeout::TimeoutLayer;

pub mod layer;

pub struct ClientBuilder<IL, OL, MkClient, Req, Resp, MkT, MkC, LB> {
    config: Config,
    streaming_options: crate::streaming::open::OpenOptions,
    pool: Option<pool::Config>,
    callee_name: FastStr,
    caller_name: FastStr,
    address: Option<Address>, // maybe address use Arc avoid memory alloc
    inner_layer: IL,
    outer_layer: OL,
    make_transport: MkT,
    make_codec: MkC,
    mk_client: MkClient,
    mk_lb: LB,
    _marker: PhantomData<(*const Req, *const Resp)>,

    disable_timeout_layer: bool,
    enable_biz_error: bool,

    #[cfg(feature = "multiplex")]
    multiplex: bool,
}

impl<C, Req, Resp>
    ClientBuilder<
        Identity,
        Identity,
        C,
        Req,
        Resp,
        DefaultMakeTransport,
        DefaultMakeCodec<MakeTTHeaderCodec<MakeFramedCodec<MakeThriftCodec>>>,
        LbConfig<WeightedRandomBalance<<DummyDiscover as Discover>::Key>, DummyDiscover>,
    >
{
    pub fn new(service_name: impl AsRef<str>, service_client: C) -> Self {
        ClientBuilder {
            config: Default::default(),
            streaming_options: Default::default(),
            pool: None,
            caller_name: "".into(),
            callee_name: FastStr::new(service_name),
            address: None,
            inner_layer: Identity::new(),
            outer_layer: Identity::new(),
            mk_client: service_client,
            make_transport: DefaultMakeTransport::default(),
            make_codec: DefaultMakeCodec::default(),
            mk_lb: LbConfig::new(WeightedRandomBalance::new(), DummyDiscover {}),
            _marker: PhantomData,

            disable_timeout_layer: false,
            enable_biz_error: true,

            #[cfg(feature = "multiplex")]
            multiplex: false,
        }
    }
}

impl<IL, OL, C, Req, Resp, MkT, MkC, LB, DISC>
    ClientBuilder<IL, OL, C, Req, Resp, MkT, MkC, LbConfig<LB, DISC>>
{
    pub fn load_balance<NLB>(
        self,
        load_balance: NLB,
    ) -> ClientBuilder<IL, OL, C, Req, Resp, MkT, MkC, LbConfig<NLB, DISC>> {
        ClientBuilder {
            config: self.config,
            streaming_options: self.streaming_options,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec: self.make_codec,
            mk_lb: self.mk_lb.load_balance(load_balance),

            disable_timeout_layer: self.disable_timeout_layer,
            enable_biz_error: self.enable_biz_error,

            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
        }
    }

    pub fn discover<NDISC>(
        self,
        discover: NDISC,
    ) -> ClientBuilder<IL, OL, C, Req, Resp, MkT, MkC, LbConfig<LB, NDISC>> {
        ClientBuilder {
            config: self.config,
            streaming_options: self.streaming_options,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec: self.make_codec,
            mk_lb: self.mk_lb.discover(discover),

            disable_timeout_layer: self.disable_timeout_layer,
            enable_biz_error: self.enable_biz_error,

            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
        }
    }

    /// Sets the retry count of the client.
    pub fn retry_count(mut self, count: usize) -> Self {
        self.mk_lb = self.mk_lb.retry_count(count);
        self
    }
}

impl<IL, OL, C, Req, Resp, MkT, MkC, LB> ClientBuilder<IL, OL, C, Req, Resp, MkT, MkC, LB> {
    pub fn streaming_hook(mut self, hook: Arc<dyn crate::streaming::StreamHook>) -> Self {
        self.streaming_options
            .hooks
            .get_or_insert_with(Default::default)
            .push(hook);
        self
    }
    /// Enables the streaming transport. Generated streaming services call this automatically.
    /// Handwritten and generic clients must opt in explicitly before building.
    pub fn streaming_protocol(mut self, _protocol: crate::streaming::StreamingProtocol) -> Self {
        self.streaming_options.enabled = true;
        self
    }
    /// Whether a generated streaming method or an explicit protocol/service enabled streaming.
    /// Hooks, layers and configuration alone do not enable the additional transport stack.
    pub fn streaming_enabled(&self) -> bool {
        self.streaming_options.enabled
    }
    /// Configures streaming without enabling it for a pure unary client.
    pub fn streaming_config(mut self, config: crate::streaming::StreamConfig) -> Self {
        self.streaming_options.config = config;
        self
    }
    pub fn streaming_hooks(mut self, hooks: crate::streaming::HookChain) -> Self {
        self.streaming_options.hooks = (!hooks.is_empty()).then_some(hooks);
        self
    }
    /// Installs a custom open service and enables streaming.
    pub fn streaming_service<Svc>(mut self, service: Svc) -> Self
    where
        Svc: Service<
                crate::streaming::StreamContext,
                crate::streaming::OpenStream,
                Response = crate::streaming::OpenedStream,
                Error = crate::streaming::StreamError,
            > + Send
            + Sync
            + 'static,
    {
        self.streaming_options.enabled = true;
        self.streaming_options.service = Some(crate::streaming::OpenService::new(service));
        self
    }
    pub fn streaming_layer<L>(mut self, layer: L) -> Self
    where
        L: Layer<crate::streaming::OpenService> + Send + 'static,
        L::Service: Service<
                crate::streaming::StreamContext,
                crate::streaming::OpenStream,
                Response = crate::streaming::OpenedStream,
                Error = crate::streaming::StreamError,
            > + Send
            + Sync
            + 'static,
    {
        self.streaming_options.layer(layer);
        self
    }

    /// Prepends an opening layer: with `foo -> bar`, adding `baz` gives
    /// `baz -> foo -> bar`. This does not wrap message send/receive operations.
    pub fn streaming_layer_front<L>(mut self, layer: L) -> Self
    where
        L: Layer<crate::streaming::OpenService> + Send + 'static,
        L::Service: Service<
                crate::streaming::StreamContext,
                crate::streaming::OpenStream,
                Response = crate::streaming::OpenedStream,
                Error = crate::streaming::StreamError,
            > + Send
            + Sync
            + 'static,
    {
        self.streaming_options.layer_front(layer);
        self
    }

    /// Sets the rpc timeout for the client.
    ///
    /// The default value is 1 second.
    ///
    /// Users can set this to `None` to disable the timeout.
    pub fn rpc_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.config.set_rpc_timeout(timeout);
        self.streaming_options.config.deadline = timeout;
        self
    }

    /// Sets the config for connection pool.
    pub fn pool_config(mut self, config: pool::Config) -> Self {
        self.pool = Some(config);
        self
    }

    /// Sets the connect timeout for the client.
    pub fn connect_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.config.set_connect_timeout(timeout);
        self
    }

    /// Sets the read write timeout for the client(a.k.a. IO timeout).
    pub fn read_write_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.config.set_read_write_timeout(timeout);
        self
    }

    /// Sets the client's name sent to the server.
    pub fn caller_name(mut self, name: impl AsRef<str>) -> Self {
        self.caller_name = FastStr::new(name);
        self
    }

    /// Disable the default timeout layer.
    #[doc(hidden)]
    pub fn disable_timeout_layer(mut self) -> Self {
        self.disable_timeout_layer = true;
        self
    }

    /// Set if to read biz error.
    ///
    /// Default is true.
    #[doc(hidden)]
    pub fn enable_biz_error(mut self, enable_biz_error: bool) -> Self {
        self.enable_biz_error = enable_biz_error;
        self
    }

    pub fn mk_load_balance<NLB>(
        self,
        mk_load_balance: NLB,
    ) -> ClientBuilder<IL, OL, C, Req, Resp, MkT, MkC, NLB> {
        ClientBuilder {
            config: self.config,
            streaming_options: self.streaming_options,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec: self.make_codec,
            mk_lb: mk_load_balance,

            disable_timeout_layer: self.disable_timeout_layer,
            enable_biz_error: self.enable_biz_error,

            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
        }
    }

    /// Set the codec to use for the client.
    ///
    /// This should not be used by most users, Volo has already provided a default encoder.
    /// This is only useful if you want to customize some protocol.
    ///
    /// If you only want to transform metadata across microservices, you can use [`metainfo`] to do
    /// this.
    #[doc(hidden)]
    pub fn make_codec<MakeCodec>(
        self,
        make_codec: MakeCodec,
    ) -> ClientBuilder<IL, OL, C, Req, Resp, MkT, MakeCodec, LB> {
        ClientBuilder {
            config: self.config,
            streaming_options: self.streaming_options,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec,
            mk_lb: self.mk_lb,

            disable_timeout_layer: self.disable_timeout_layer,
            enable_biz_error: self.enable_biz_error,

            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
        }
    }

    /// Set the transport to use for the client.
    #[doc(hidden)]
    pub fn make_transport<MakeTransport>(
        self,
        make_transport: MakeTransport,
    ) -> ClientBuilder<IL, OL, C, Req, Resp, MakeTransport, MkC, LB> {
        ClientBuilder {
            config: self.config,
            streaming_options: self.streaming_options,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport,
            make_codec: self.make_codec,
            mk_lb: self.mk_lb,

            disable_timeout_layer: self.disable_timeout_layer,
            enable_biz_error: self.enable_biz_error,

            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
        }
    }

    /// Sets the target address.
    ///
    /// If the address is set, the call will be sent to the address directly.
    ///
    /// The client will skip the discovery and loadbalance Service if this is set.
    pub fn address<A: Into<Address>>(mut self, target: A) -> Self {
        self.address = Some(target.into());
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
    ) -> ClientBuilder<Stack<Inner, IL>, OL, C, Req, Resp, MkT, MkC, LB> {
        ClientBuilder {
            config: self.config,
            streaming_options: self.streaming_options,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            inner_layer: Stack::new(layer, self.inner_layer),
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec: self.make_codec,
            mk_lb: self.mk_lb,

            disable_timeout_layer: self.disable_timeout_layer,
            enable_biz_error: self.enable_biz_error,

            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
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
    ) -> ClientBuilder<Stack<IL, Inner>, OL, C, Req, Resp, MkT, MkC, LB> {
        ClientBuilder {
            config: self.config,
            streaming_options: self.streaming_options,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            inner_layer: Stack::new(self.inner_layer, layer),
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec: self.make_codec,
            mk_lb: self.mk_lb,

            disable_timeout_layer: self.disable_timeout_layer,
            enable_biz_error: self.enable_biz_error,

            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
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
    /// The overall order for layers is: \[outer\] -> Timeout -> LoadBalance -> inner -> transport.
    pub fn layer_outer<Outer>(
        self,
        layer: Outer,
    ) -> ClientBuilder<IL, Stack<Outer, OL>, C, Req, Resp, MkT, MkC, LB> {
        ClientBuilder {
            config: self.config,
            streaming_options: self.streaming_options,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            inner_layer: self.inner_layer,
            outer_layer: Stack::new(layer, self.outer_layer),
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec: self.make_codec,
            mk_lb: self.mk_lb,

            disable_timeout_layer: self.disable_timeout_layer,
            enable_biz_error: self.enable_biz_error,

            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
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
    /// The overall order for layers is: \[outer\] -> Timeout -> LoadBalance -> inner -> transport.
    pub fn layer_outer_front<Outer>(
        self,
        layer: Outer,
    ) -> ClientBuilder<IL, Stack<OL, Outer>, C, Req, Resp, MkT, MkC, LB> {
        ClientBuilder {
            config: self.config,
            streaming_options: self.streaming_options,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            inner_layer: self.inner_layer,
            outer_layer: Stack::new(self.outer_layer, layer),
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec: self.make_codec,
            mk_lb: self.mk_lb,

            disable_timeout_layer: self.disable_timeout_layer,
            enable_biz_error: self.enable_biz_error,

            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
        }
    }

    #[cfg(feature = "multiplex")]
    /// Enable multiplexing for the client.
    #[doc(hidden)]
    pub fn multiplex(self, multiplex: bool) -> ClientBuilder<IL, OL, C, Req, Resp, MkT, MkC, LB> {
        ClientBuilder {
            config: self.config,
            streaming_options: self.streaming_options,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec: self.make_codec,
            mk_lb: self.mk_lb,

            disable_timeout_layer: self.disable_timeout_layer,
            enable_biz_error: self.enable_biz_error,

            multiplex,
        }
    }

    #[doc(hidden)]
    pub fn get_callee_name(&self) -> &FastStr {
        &self.callee_name
    }
}

#[derive(Clone)]
pub struct MessageService<Resp, MkT, MkC>
where
    Resp: EntryMessage + Send + 'static,
    MkT: MakeTransport,
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
{
    #[cfg(not(feature = "multiplex"))]
    inner: pingpong::Client<Resp, MkT, MkC>,
    #[cfg(feature = "multiplex")]
    inner: motore::utils::Either<
        pingpong::Client<Resp, MkT, MkC>,
        crate::transport::multiplex::Client<Resp, MkT, MkC>,
    >,
    read_biz_error: bool,
}

impl<Req, Resp, MkT, MkC> Service<ClientContext, Req> for MessageService<Resp, MkT, MkC>
where
    Req: EntryMessage + 'static + Send,
    Resp: Send + 'static + EntryMessage + Sync,
    MkT: MakeTransport,
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
{
    type Response = Option<Resp>;

    type Error = ClientError;

    async fn call(&self, cx: &mut ClientContext, req: Req) -> Result<Self::Response, Self::Error> {
        let msg = ThriftMessage::mk_client_msg(cx, req);
        let resp = self.inner.call(cx, msg).await;
        if self.read_biz_error {
            if let Some(biz_err) = cx.common_stats.biz_error() {
                return Err(biz_err.clone().into());
            }
        }
        match resp {
            Ok(Some(ThriftMessage { data: Ok(data), .. })) => Ok(Some(data)),
            Ok(Some(ThriftMessage { data: Err(e), .. })) => Err(e.into()),
            Err(e) => Err(e),
            Ok(None) => Ok(None),
        }
    }
}

impl<IL, OL, C, Req, Resp, MkT, MkC, LB> ClientBuilder<IL, OL, C, Req, Resp, MkT, MkC, LB>
where
    C: volo::client::MkClient<
            Client<
                BoxCloneService<
                    ClientContext,
                    Req,
                    Option<Resp>,
                    <OL::Service as Service<ClientContext, Req>>::Error,
                >,
            >,
        >,
    LB: MkLbLayer,
    LB::Layer: Layer<IL::Service>,
    <LB::Layer as Layer<IL::Service>>::Service: Service<ClientContext, Req, Response = Option<Resp>, Error = ClientError>
        + 'static
        + Send
        + Clone
        + Sync,
    Req: EntryMessage + Send + 'static + Sync + Clone,
    Resp: EntryMessage + Send + 'static,
    IL: Layer<MessageService<Resp, MkT, MkC>>,
    IL::Service:
        Service<ClientContext, Req, Response = Option<Resp>> + Sync + Clone + Send + 'static,
    <IL::Service as Service<ClientContext, Req>>::Error: Send + Into<ClientError>,
    MkT: MakeTransport,
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
    OL: Layer<BoxCloneService<ClientContext, Req, Option<Resp>, ClientError>>,
    OL::Service:
        Service<ClientContext, Req, Response = Option<Resp>> + 'static + Send + Clone + Sync,
    <OL::Service as Service<ClientContext, Req>>::Error: Send + Sync + Into<ClientError>,
{
    /// Build volo client.
    pub fn build(mut self) -> C::Target {
        if let Some(timeout) = self.config.connect_timeout() {
            self.make_transport.set_connect_timeout(Some(timeout));
        }
        if let Some(timeout) = self.config.read_write_timeout() {
            self.make_transport.set_read_timeout(Some(timeout));
        }
        if let Some(timeout) = self.config.read_write_timeout() {
            self.make_transport.set_write_timeout(Some(timeout));
        }
        let (lb_layer, streaming) = if self.streaming_options.enabled {
            let (layer, resolver) = self.mk_lb.make_with_resolver();
            let streaming = self
                .streaming_options
                .build(self.make_transport.clone(), resolver);
            (layer, Some(streaming))
        } else {
            (self.mk_lb.make(), None)
        };
        let msg_svc = MessageService {
            #[cfg(not(feature = "multiplex"))]
            inner: pingpong::Client::new(self.make_transport, self.pool, self.make_codec),
            #[cfg(feature = "multiplex")]
            inner: if !self.multiplex {
                motore::utils::Either::A(pingpong::Client::new(
                    self.make_transport,
                    self.pool,
                    self.make_codec,
                ))
            } else {
                motore::utils::Either::B(crate::transport::multiplex::Client::new(
                    self.make_transport,
                    self.pool,
                    self.make_codec,
                ))
            },
            read_biz_error: self.enable_biz_error,
        };

        let transport = if !self.disable_timeout_layer {
            BoxCloneService::new(self.outer_layer.layer(BoxCloneService::new(
                TimeoutLayer::new().layer(lb_layer.layer(self.inner_layer.layer(msg_svc))),
            )))
        } else {
            BoxCloneService::new(self.outer_layer.layer(BoxCloneService::new(
                lb_layer.layer(self.inner_layer.layer(msg_svc)),
            )))
        };

        self.mk_client.mk_client(Client {
            inner: Arc::new(ClientInner {
                callee_name: self.callee_name,
                config: self.config,
                streaming,
                address: self.address,
                caller_name: self.caller_name,
                seq_id: AtomicI32::new(0),
            }),
            transport,
        })
    }
}

/// A client for a Thrift service.
///
/// `Client` is designed to "clone and use", so it's cheap to clone it.
/// One important thing is that the `CallOpt` will not be cloned, because
/// it's designed to be per-request.
#[derive(Clone)]
pub struct Client<S> {
    transport: S,
    inner: Arc<ClientInner>,
}

// unsafe impl<Req, Resp> Sync for Client<Req, Resp> {}

struct ClientInner {
    streaming: Option<crate::streaming::OpenService>,
    callee_name: FastStr,
    caller_name: FastStr,
    config: Config,
    address: Option<Address>,
    seq_id: AtomicI32,
}

impl<S> Client<S> {
    pub async fn open_stream(
        &self,
        service: &str,
        method: &str,
    ) -> Result<crate::streaming::OpenedStream, crate::streaming::StreamError> {
        self.open_stream_with_mode(service, method, crate::streaming::StreamMode::Raw)
            .await
    }
    pub async fn open_stream_with_mode(
        &self,
        service: &str,
        method: &str,
        mode: crate::streaming::StreamMode,
    ) -> Result<crate::streaming::OpenedStream, crate::streaming::StreamError> {
        if !cfg!(feature = "ttstream") {
            return Err(crate::streaming::StreamError::configuration(
                "ttstream feature is disabled",
            ));
        }
        if self.inner.streaming.is_none() {
            return Err(crate::streaming::StreamError::configuration(
                "streaming is not enabled; configure streaming_protocol(TtHeader)",
            ));
        }
        let mut cx = crate::streaming::StreamContext::new(
            crate::transport::ttstream::connection::next_id(),
            service,
            method,
            Role::Client,
        );
        cx.set_mode(mode);
        cx.update_rpc_info(|rpc| *rpc = self.make_rpc_info(method));
        open_stream_context(&self.inner, cx).await
    }

    /// Consumes an ordinary unary CallOpt before entering the streaming open
    /// stack. The option and its non-Clone endpoint tags are moved exactly once.
    pub async fn open_stream_with_callopt(
        self,
        service: &str,
        method: &str,
        mode: crate::streaming::StreamMode,
    ) -> Result<crate::streaming::OpenedStream, crate::streaming::StreamError>
    where
        S: volo::client::Apply<ClientContext, Error = ClientError>,
    {
        if !cfg!(feature = "ttstream") {
            return Err(crate::streaming::StreamError::configuration(
                "ttstream feature is disabled",
            ));
        }
        if self.inner.streaming.is_none() {
            return Err(crate::streaming::StreamError::configuration(
                "streaming is not enabled; configure streaming_protocol(TtHeader)",
            ));
        }
        let mut cx = crate::streaming::StreamContext::new(
            crate::transport::ttstream::connection::next_id(),
            service,
            method,
            Role::Client,
        );
        cx.set_mode(mode);
        let mut call = self.make_cx(method, false);
        if let Some(timeout) =
            <S as volo::client::Apply<ClientContext>>::rpc_timeout(&self.transport)
        {
            cx.set_deadline(Some(cx.started_at() + timeout));
        }
        if let Err(error) = self.transport.apply(&mut call) {
            // The outer open guard still owns failure observation and one finish.
            cx.cancel(crate::streaming::StreamError::configuration(
                error.to_string(),
            ));
        }
        cx.update_rpc_info(|rpc| std::mem::swap(rpc, call.rpc_info_mut()));
        open_stream_context(&self.inner, cx).await
    }

    pub fn make_cx(&self, method: &str, oneway: bool) -> ClientContext {
        CLIENT_CONTEXT_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            cache
                .pop()
                .map(|mut cx| {
                    // The generated code only push the cx to the cache, we need to reset
                    // it after we pop it from the cache.
                    cx.reset(
                        self.inner
                            .seq_id
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                        if oneway {
                            TMessageType::OneWay
                        } else {
                            TMessageType::Call
                        },
                    );
                    // reset rpc_info
                    cx.rpc_info_mut()
                        .caller_mut()
                        .set_service_name(self.inner.caller_name.clone());
                    cx.rpc_info_mut()
                        .callee_mut()
                        .set_service_name(self.inner.callee_name.clone());
                    if let Some(target) = &self.inner.address {
                        cx.rpc_info_mut().callee_mut().set_address(target.clone());
                    }
                    cx.rpc_info_mut().set_config(self.inner.config);
                    cx.rpc_info_mut().set_method(FastStr::new(method));
                    cx
                })
                .unwrap_or_else(|| {
                    ClientContext::new(
                        self.inner
                            .seq_id
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                        self.make_rpc_info(method),
                        if oneway {
                            TMessageType::OneWay
                        } else {
                            TMessageType::Call
                        },
                    )
                })
        })
    }

    fn make_rpc_info(&self, method: &str) -> RpcInfo<Config> {
        let caller = Endpoint::new(self.inner.caller_name.clone());
        let mut callee = Endpoint::new(self.inner.callee_name.clone());
        if let Some(target) = &self.inner.address {
            callee.set_address(target.clone());
        }
        let config = self.inner.config;

        RpcInfo::new(Role::Client, FastStr::new(method), caller, callee, config)
    }

    pub fn with_opt<Opt>(self, opt: Opt) -> Client<WithOptService<S, Opt>> {
        Client {
            transport: WithOptService::new(self.transport, opt),
            inner: self.inner,
        }
    }
}

async fn open_stream_context(
    inner: &ClientInner,
    mut cx: crate::streaming::StreamContext,
) -> Result<crate::streaming::OpenedStream, crate::streaming::StreamError> {
    let _ = crate::streaming::STREAM_CONTEXT.try_with(|parent| parent.attach_child(&cx));
    if crate::streaming::STREAM_CONTEXT
        .try_with(|parent| {
            parent.with_metainfo(|info| {
                cx.update_metainfo(|target| crate::streaming::copy_outgoing_metainfo(info, target))
            })
        })
        .is_err()
    {
        let _ = metainfo::METAINFO.try_with(|info| {
            cx.update_metainfo(|target| {
                crate::streaming::copy_outgoing_metainfo(&info.borrow(), target)
            })
        });
    }
    let request = crate::streaming::OpenStream {
        address: cx.with_rpc_info(|rpc| rpc.callee().address()),
        metadata: Default::default(),
    };
    inner
        .streaming
        .as_ref()
        .ok_or_else(|| {
            crate::streaming::StreamError::configuration(
                "streaming is not enabled; configure streaming_protocol(TtHeader)",
            )
        })?
        .call(&mut cx, request)
        .await
}

macro_rules! impl_client {
    (($self: ident, &mut $cx:ident, $req: ident) => async move $e: tt ) => {
        impl<S, Req: Send + 'static, Res: 'static>
            volo::service::Service<crate::context::ClientContext, Req> for Client<S>
        where
            S: volo::service::Service<
                    crate::context::ClientContext,
                    Req,
                    Response = Option<Res>,
                    Error = crate::ClientError,
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

        impl<S, Req: Send + 'static, Res: 'static>
            volo::client::OneShotService<crate::context::ClientContext, Req> for Client<S>
        where
            S: volo::client::OneShotService<
                    crate::context::ClientContext,
                    Req,
                    Response = Option<Res>,
                    Error = crate::ClientError,
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

#[cfg(test)]
mod streaming_activation_tests {
    use bytes::Bytes;

    use super::*;
    struct Make;
    impl<S> volo::client::MkClient<Client<S>> for Make {
        type Target = Client<S>;
        fn mk_client(&self, service: Client<S>) -> Client<S> {
            service
        }
    }
    #[tokio::test]
    async fn legacy_builder_and_configuration_only_do_not_construct_streaming_stack() {
        let builder = ClientBuilder::<_, _, _, Bytes, Bytes, _, _, _>::new("Unary", Make)
            .streaming_config(Default::default())
            .streaming_hooks(Default::default())
            .streaming_layer(volo::layer::Identity::new());
        assert!(!builder.streaming_enabled());
        let client = builder.build();
        assert!(client.inner.streaming.is_none());
        assert!(matches!(
            client.open_stream("Unary", "Unknown").await,
            Err(crate::streaming::StreamError::Configuration(_))
        ));
    }
    #[tokio::test]
    async fn custom_open_service_is_an_explicit_activation() {
        struct Custom;
        impl Service<crate::streaming::StreamContext, crate::streaming::OpenStream> for Custom {
            type Response = crate::streaming::OpenedStream;
            type Error = crate::streaming::StreamError;
            async fn call(
                &self,
                _: &mut crate::streaming::StreamContext,
                _: crate::streaming::OpenStream,
            ) -> Result<Self::Response, Self::Error> {
                Err(crate::streaming::StreamError::Configuration(
                    "custom open reached".into(),
                ))
            }
        }
        let builder = ClientBuilder::<_, _, _, Bytes, Bytes, _, _, _>::new("Raw", Make)
            .streaming_service(Custom);
        assert!(builder.streaming_enabled());
        let client = builder.build();
        assert!(client.inner.streaming.is_some());
        let error = client.open_stream("Raw", "Bidi").await.err().unwrap();
        if cfg!(feature = "ttstream") {
            assert!(error.to_string().contains("custom open reached"));
        } else {
            assert!(error.to_string().contains("feature is disabled"));
        }
    }
    #[tokio::test]
    async fn explicit_protocol_enables_raw_after_configuration() {
        let builder = ClientBuilder::<_, _, _, Bytes, Bytes, _, _, _>::new("Raw", Make)
            .streaming_layer(volo::layer::Identity::new())
            .streaming_protocol(crate::streaming::StreamingProtocol::TtHeader);
        assert!(builder.streaming_enabled());
        assert!(builder.build().inner.streaming.is_some());
    }
}
