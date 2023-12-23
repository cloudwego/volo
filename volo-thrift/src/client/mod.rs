//! Thrift client for Volo.
//!
//! Users should not use this module directly.
//! Instead, they should use the `Builder` type in the generated code.
//!
//! For users need to specify some options at call time, they may use ['callopt'][callopt].

use std::{
    cell::RefCell,
    marker::PhantomData,
    sync::{atomic::AtomicI32, Arc},
};

use http::{HeaderMap, header::IntoHeaderName, HeaderValue};
use motore::{
    layer::{Identity, Layer, Stack},
    service::{BoxCloneService, Service},
};
use pilota::thrift::TMessageType;
use tokio::time::Duration;
use volo::{
    client::WithOptService,
    context::{Context, Endpoint, Role, RpcInfo},
    discovery::{Discover, DummyDiscover},
    loadbalance::{random::WeightedRandomBalance, LbConfig, MkLbLayer},
    net::{
        dial::{DefaultMakeTransport, MakeTransport},
        Address,
    },
    FastStr,
};

use crate::{
    codec::{
        default::{framed::MakeFramedCodec, thrift::MakeThriftCodec, ttheader::MakeTTHeaderCodec},
        DefaultMakeCodec, MakeCodec,
    },
    context::{ClientContext, Config, CLIENT_CONTEXT_CACHE},
    error::{Error, Result},
    transport::{pingpong, pool},
    EntryMessage, ThriftMessage,
};

mod callopt;
pub use callopt::CallOpt;

use self::layer::timeout::TimeoutLayer;

pub mod layer;

pub struct ClientBuilder<IL, OL, MkClient, Req, Resp, MkT, MkC, LB> {
    config: Config,
    pool: Option<pool::Config>,
    callee_name: FastStr,
    caller_name: FastStr,
    address: Option<Address>, // maybe address use Arc avoid memory alloc
    headers: Option<HeaderMap>,
    inner_layer: IL,
    outer_layer: OL,
    make_transport: MkT,
    make_codec: MkC,
    mk_client: MkClient,
    mk_lb: LB,
    _marker: PhantomData<(*const Req, *const Resp)>,

    disable_timeout_layer: bool,

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
        // MkT,
        DefaultMakeTransport,

        DefaultMakeCodec<MakeTTHeaderCodec<MakeFramedCodec<MakeThriftCodec>>>,
        LbConfig<WeightedRandomBalance<<DummyDiscover as Discover>::Key>, DummyDiscover>,
    >
// where
//     MkT: MakeTransport + Default,
{
    pub fn new(service_name: impl AsRef<str>, service_client: C) -> Self {
        ClientBuilder {
            config: Default::default(),
            pool: None,
            caller_name: "".into(),
            callee_name: FastStr::new(service_name),
            address: None,
            headers: None,
            inner_layer: Identity::new(),
            outer_layer: Identity::new(),
            mk_client: service_client,
            // make_transport: MkT::default(),
            make_transport: DefaultMakeTransport::default(),

            make_codec: DefaultMakeCodec::default(),
            mk_lb: LbConfig::new(WeightedRandomBalance::new(), DummyDiscover {}),
            _marker: PhantomData,

            disable_timeout_layer: false,

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
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec: self.make_codec,
            mk_lb: self.mk_lb.load_balance(load_balance),

            disable_timeout_layer: self.disable_timeout_layer,

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
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec: self.make_codec,
            mk_lb: self.mk_lb.discover(discover),

            disable_timeout_layer: self.disable_timeout_layer,

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
    /// Sets the rpc timeout for the client.
    ///
    /// The default value is 1 second.
    ///
    /// Users can set this to `None` to disable the timeout.
    pub fn rpc_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.config.set_rpc_timeout(timeout);
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

    pub fn mk_load_balance<NLB>(
        self,
        mk_load_balance: NLB,
    ) -> ClientBuilder<IL, OL, C, Req, Resp, MkT, MkC, NLB> {
        ClientBuilder {
            config: self.config,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec: self.make_codec,
            mk_lb: mk_load_balance,

            disable_timeout_layer: self.disable_timeout_layer,

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
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec,
            mk_lb: self.mk_lb,

            disable_timeout_layer: self.disable_timeout_layer,

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
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport,
            make_codec: self.make_codec,
            mk_lb: self.mk_lb,

            disable_timeout_layer: self.disable_timeout_layer,

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

    /// Add transport header
    /// 
    pub fn header<K: IntoHeaderName>(mut self, key: K, value: HeaderValue) -> Self {
        if let Some(existing) = &mut self.headers {
            existing.append(key, value);
        } else {
            let mut headers = HeaderMap::new();
            headers.append(key, value);
            self.headers = Some(headers);
        }
        self
    }

    /// Add transport headers
    /// 
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        if let Some(existing) = &mut self.headers {
            existing.extend(headers);
        } else {
            self.headers = Some(headers);
        }
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
    /// The overall order for layers is: outer -> LoadBalance -> [inner] -> transport.
    pub fn layer_inner<Inner>(
        self,
        layer: Inner,
    ) -> ClientBuilder<Stack<Inner, IL>, OL, C, Req, Resp, MkT, MkC, LB> {
        ClientBuilder {
            config: self.config,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            headers: self.headers,
            inner_layer: Stack::new(layer, self.inner_layer),
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec: self.make_codec,
            mk_lb: self.mk_lb,

            disable_timeout_layer: self.disable_timeout_layer,

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
    /// The overall order for layers is: Timeout -> [outer] -> LoadBalance -> inner -> transport.
    pub fn layer_outer<Outer>(
        self,
        layer: Outer,
    ) -> ClientBuilder<IL, Stack<Outer, OL>, C, Req, Resp, MkT, MkC, LB> {
        ClientBuilder {
            config: self.config,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: Stack::new(layer, self.outer_layer),
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec: self.make_codec,
            mk_lb: self.mk_lb,

            disable_timeout_layer: self.disable_timeout_layer,

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
    /// The overall order for layers is: [outer] -> Timeout -> LoadBalance -> inner -> transport.
    pub fn layer_outer_front<Outer>(
        self,
        layer: Outer,
    ) -> ClientBuilder<IL, Stack<OL, Outer>, C, Req, Resp, MkT, MkC, LB> {
        ClientBuilder {
            config: self.config,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: Stack::new(self.outer_layer, layer),
            mk_client: self.mk_client,
            _marker: PhantomData,
            make_transport: self.make_transport,
            make_codec: self.make_codec,
            mk_lb: self.mk_lb,

            disable_timeout_layer: self.disable_timeout_layer,

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
}

impl<Req, Resp, MkT, MkC> Service<ClientContext, Req> for MessageService<Resp, MkT, MkC>
where
    Req: EntryMessage + 'static + Send,
    Resp: Send + 'static + EntryMessage + Sync,
    MkT: MakeTransport,
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
{
    type Response = Option<Resp>;

    type Error = Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut ClientContext,
        req: Req,
    ) -> Result<Self::Response, Self::Error> {
        let msg = ThriftMessage::mk_client_msg(cx, Ok(req))?;
        let resp = self.inner.call(cx, msg).await;
        match resp {
            Ok(Some(ThriftMessage { data: Ok(data), .. })) => Ok(Some(data)),
            Ok(Some(ThriftMessage { data: Err(e), .. })) => Err(e),
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
    <LB::Layer as Layer<IL::Service>>::Service: Service<ClientContext, Req, Response = Option<Resp>, Error = Error>
        + 'static
        + Send
        + Clone
        + Sync,
    Req: EntryMessage + Send + 'static + Sync + Clone,
    Resp: EntryMessage + Send + 'static,
    IL: Layer<MessageService<Resp, MkT, MkC>>,
    IL::Service:
        Service<ClientContext, Req, Response = Option<Resp>> + Sync + Clone + Send + 'static,
    <IL::Service as Service<ClientContext, Req>>::Error: Send + Into<Error>,
    MkT: MakeTransport,
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
    OL: Layer<BoxCloneService<ClientContext, Req, Option<Resp>, Error>>,
    OL::Service:
        Service<ClientContext, Req, Response = Option<Resp>> + 'static + Send + Clone + Sync,
    <OL::Service as Service<ClientContext, Req>>::Error: Send + Sync + Into<Error>,
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
        if let Some(headers) = self.headers {
            self.make_transport.set_headers(Some(headers))
        }

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
        };

        let transport = if !self.disable_timeout_layer {
            BoxCloneService::new(self.outer_layer.layer(BoxCloneService::new(
                TimeoutLayer::new().layer(self.mk_lb.make().layer(self.inner_layer.layer(msg_svc))),
            )))
        } else {
            BoxCloneService::new(self.outer_layer.layer(BoxCloneService::new(
                self.mk_lb.make().layer(self.inner_layer.layer(msg_svc)),
            )))
        };

        self.mk_client.mk_client(Client {
            inner: Arc::new(ClientInner {
                callee_name: self.callee_name,
                config: self.config,
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
    callee_name: FastStr,
    caller_name: FastStr,
    config: Config,
    address: Option<Address>,
    seq_id: AtomicI32,
}

impl<S> Client<S> {
    pub fn make_cx(&self, method: &'static str, oneway: bool) -> ClientContext {
        CLIENT_CONTEXT_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            cache
                .pop()
                .and_then(|mut cx| {
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
                    cx.rpc_info_mut()
                        .set_method(FastStr::from_static_str(method));
                    Some(cx)
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

    fn make_rpc_info(&self, method: &'static str) -> RpcInfo<Config> {
        let caller = Endpoint::new(self.inner.caller_name.clone());
        let mut callee = Endpoint::new(self.inner.callee_name.clone());
        if let Some(target) = &self.inner.address {
            callee.set_address(target.clone());
        }
        let config = self.inner.config;

        RpcInfo::new(Role::Client, method.into(), caller, callee, config)
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
        impl<S, Req: Send + 'static, Res: 'static>
            volo::service::Service<crate::context::ClientContext, Req> for Client<S>
        where
            S: volo::service::Service<
                    crate::context::ClientContext,
                    Req,
                    Response = Option<Res>,
                    Error = crate::Error,
                > + Sync
                + Send
                + 'static,
        {
            type Response = S::Response;
            type Error = S::Error;

            async fn call<'s, 'cx>(
                &'s $self,
                $cx: &'cx mut crate::context::ClientContext,
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
                    Error = crate::Error,
                > + Sync
                + Send
                + 'static,
        {
            type Response = S::Response;
            type Error = S::Error;

            async fn call<'cx>(
                $self,
                $cx: &'cx mut crate::context::ClientContext,
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
