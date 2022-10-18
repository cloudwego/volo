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

use futures::Future;
use motore::{
    layer::{Identity, Layer, Stack},
    service::{BoxCloneService, Service},
};
use pilota::thrift::TMessageType;
use tokio::time::Duration;
use volo::{
    context::{Context, Endpoint, Role, RpcInfo},
    discovery::{Discover, DummyDiscover},
    loadbalance::{random::WeightedRandomBalance, LbConfig, MkLbLayer},
    net::{dial::MakeConnection, Address},
};

use crate::{
    codec::{
        tt_header::DefaultTTHeaderCodec, CodecType, MakeClientDecoder, MakeClientEncoder,
        MkDecoder, MkEncoder,
    },
    context::{ClientContext, Config},
    error::{Error, Result},
    tags::TransportType,
    transport::{pingpong, pool},
    EntryMessage, ThriftMessage,
};

mod callopt;
pub use callopt::CallOpt;

use self::layer::timeout::TimeoutLayer;

pub mod layer;

/// Only used by framework generated code.
/// Do not use directly.
#[doc(hidden)]
pub trait SetClient<Req, Resp> {
    fn set_client(self, client: Client<Req, Resp>) -> Self;
}

pub struct ClientBuilder<IL, OL, C, Req, Resp, MkE, MkD, LB> {
    config: Config,
    pool: Option<pool::Config>,
    callee_name: smol_str::SmolStr,
    caller_name: smol_str::SmolStr,
    address: Option<Address>, // maybe address use Arc avoid memory alloc
    inner_layer: IL,
    outer_layer: OL,
    codec_type: CodecType,
    service_client: C,
    mk_encoder: MkE,
    mk_decoder: MkD,
    mk_lb: LB,
    _marker: PhantomData<(*const Req, *const Resp)>,

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
        MakeClientEncoder<DefaultTTHeaderCodec>,
        MakeClientDecoder<DefaultTTHeaderCodec>,
        LbConfig<WeightedRandomBalance<<DummyDiscover as Discover>::Key>, DummyDiscover>,
    >
where
    C: SetClient<Req, Resp>,
{
    pub fn new(service_name: impl AsRef<str>, service_client: C) -> Self {
        ClientBuilder {
            config: Default::default(),
            pool: None,
            caller_name: "".into(),
            callee_name: service_name.into(),
            address: None,
            inner_layer: Identity::new(),
            outer_layer: Identity::new(),
            codec_type: CodecType::TTHeaderFramed,
            service_client,
            mk_encoder: MakeClientEncoder {
                tt_encoder: DefaultTTHeaderCodec,
            },
            mk_decoder: MakeClientDecoder {
                tt_decoder: DefaultTTHeaderCodec,
            },
            mk_lb: LbConfig::new(WeightedRandomBalance::new(), DummyDiscover {}),
            _marker: PhantomData,

            #[cfg(feature = "multiplex")]
            multiplex: false,
        }
    }
}

impl<IL, OL, C, Req, Resp, E, D, LB, DISC>
    ClientBuilder<IL, OL, C, Req, Resp, E, D, LbConfig<LB, DISC>>
where
    C: SetClient<Req, Resp>,
{
    pub fn load_balance<NLB>(
        self,
        load_balance: NLB,
    ) -> ClientBuilder<IL, OL, C, Req, Resp, E, D, LbConfig<NLB, DISC>> {
        ClientBuilder {
            config: self.config,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            codec_type: self.codec_type,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            service_client: self.service_client,
            _marker: PhantomData,
            mk_encoder: self.mk_encoder,
            mk_decoder: self.mk_decoder,
            mk_lb: self.mk_lb.load_balance(load_balance),

            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
        }
    }

    pub fn discover<NDISC>(
        self,
        discover: NDISC,
    ) -> ClientBuilder<IL, OL, C, Req, Resp, E, D, LbConfig<LB, NDISC>> {
        ClientBuilder {
            config: self.config,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            codec_type: self.codec_type,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            service_client: self.service_client,
            _marker: PhantomData,
            mk_encoder: self.mk_encoder,
            mk_decoder: self.mk_decoder,
            mk_lb: self.mk_lb.discover(discover),

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

impl<IL, OL, C, Req, Resp, E, D, LB> ClientBuilder<IL, OL, C, Req, Resp, E, D, LB>
where
    C: SetClient<Req, Resp>,
{
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

    /// Sets the max frame size for the client.
    ///
    /// Defaults to 16MB.
    pub fn max_frame_size(mut self, max_frame_size: u32) -> Self {
        self.config.set_max_frame_size(max_frame_size);
        self
    }

    /// Sets the client's name sent to the server.
    pub fn caller_name(mut self, name: impl AsRef<str>) -> Self {
        self.caller_name = name.into();
        self
    }

    pub fn mk_load_balance<NLB>(
        self,
        mk_load_balance: NLB,
    ) -> ClientBuilder<IL, OL, C, Req, Resp, E, D, NLB> {
        ClientBuilder {
            config: self.config,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            codec_type: self.codec_type,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            service_client: self.service_client,
            _marker: PhantomData,
            mk_encoder: self.mk_encoder,
            mk_decoder: self.mk_decoder,
            mk_lb: mk_load_balance,

            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
        }
    }

    /// Set the TTHeader encoder to use for the client.
    ///
    /// This should not be used by most users, Volo has already provided a default encoder.
    /// This is only useful if you want to customize TTHeader protocol and use it together with
    /// a proxy (such as service mesh).
    ///
    /// If you only want to transform metadata across microservices, you can use [`metainfo`] to do
    /// this.
    #[doc(hidden)]
    pub fn tt_header_encoder<TTEncoder>(
        self,
        tt_encoder: TTEncoder,
    ) -> ClientBuilder<IL, OL, C, Req, Resp, MakeClientEncoder<TTEncoder>, D, LB> {
        ClientBuilder {
            config: self.config,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            codec_type: self.codec_type,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            service_client: self.service_client,
            _marker: PhantomData,
            mk_encoder: MakeClientEncoder { tt_encoder },
            mk_decoder: self.mk_decoder,
            mk_lb: self.mk_lb,

            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
        }
    }

    /// Set the TTHeader decoder to use for the client.
    ///
    /// This should not be used by most users, Volo has already provided a default decoder.
    /// This is only useful if you want to customize TTHeader protocol and use it together with
    /// a proxy (such as service mesh).
    ///
    /// If you only want to transform metadata across microservices, you can use [`metainfo`] to do
    /// this.
    #[doc(hidden)]
    pub fn tt_header_decoder<TTDecoder>(
        self,
        tt_decoder: TTDecoder,
    ) -> ClientBuilder<IL, OL, C, Req, Resp, E, MakeClientDecoder<TTDecoder>, LB> {
        ClientBuilder {
            config: self.config,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            codec_type: self.codec_type,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            service_client: self.service_client,
            _marker: PhantomData,
            mk_encoder: self.mk_encoder,
            mk_decoder: MakeClientDecoder { tt_decoder },
            mk_lb: self.mk_lb,

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

    /// Sets the codec type used for the client.
    ///
    /// Most users don't need to change this.
    ///
    /// Defaults to `CodecType::TTHeaderFramed`.
    pub fn codec_type(mut self, t: CodecType) -> Self {
        self.codec_type = t;
        self
    }

    /// Adds a new inner layer to the client.
    ///
    /// The layer's `Service` should be `Send + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer_inner(baz)`, we will get: foo -> bar -> baz.
    ///
    /// The overall order for layers is: Timeout -> outer -> LoadBalance -> [inner] -> transport.
    pub fn layer_inner<Inner>(
        self,
        layer: Inner,
    ) -> ClientBuilder<Stack<Inner, IL>, OL, C, Req, Resp, E, D, LB> {
        ClientBuilder {
            config: self.config,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            codec_type: self.codec_type,
            inner_layer: Stack::new(layer, self.inner_layer),
            outer_layer: self.outer_layer,
            service_client: self.service_client,
            _marker: PhantomData,
            mk_encoder: self.mk_encoder,
            mk_decoder: self.mk_decoder,
            mk_lb: self.mk_lb,

            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
        }
    }

    /// Adds a new outer layer to the client.
    ///
    /// The layer's `Service` should be `Send + Clone + 'static`.
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
    ) -> ClientBuilder<IL, Stack<Outer, OL>, C, Req, Resp, E, D, LB> {
        ClientBuilder {
            config: self.config,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            codec_type: self.codec_type,
            inner_layer: self.inner_layer,
            outer_layer: Stack::new(layer, self.outer_layer),
            service_client: self.service_client,
            _marker: PhantomData,
            mk_encoder: self.mk_encoder,
            mk_decoder: self.mk_decoder,
            mk_lb: self.mk_lb,

            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
        }
    }

    /// Adds a new outer layer to the client.
    ///
    /// The layer's `Service` should be `Send + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer_outer_front(baz)`, we will get: baz -> foo -> bar.
    ///
    /// The overall order for layers is: Timeout -> [outer] -> LoadBalance -> inner -> transport.
    pub fn layer_outer_front<Outer>(
        self,
        layer: Outer,
    ) -> ClientBuilder<IL, Stack<OL, Outer>, C, Req, Resp, E, D, LB> {
        ClientBuilder {
            config: self.config,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            codec_type: self.codec_type,
            inner_layer: self.inner_layer,
            outer_layer: Stack::new(self.outer_layer, layer),
            service_client: self.service_client,
            _marker: PhantomData,
            mk_encoder: self.mk_encoder,
            mk_decoder: self.mk_decoder,
            mk_lb: self.mk_lb,

            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
        }
    }

    #[cfg(feature = "multiplex")]
    /// Enable multiplexing for the client.
    #[doc(hidden)]
    pub fn multiplex(self, multiplex: bool) -> ClientBuilder<IL, OL, C, Req, Resp, E, D, LB> {
        ClientBuilder {
            config: self.config,
            pool: self.pool,
            caller_name: self.caller_name,
            callee_name: self.callee_name,
            address: self.address,
            codec_type: self.codec_type,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            service_client: self.service_client,
            _marker: PhantomData,
            mk_encoder: self.mk_encoder,
            mk_decoder: self.mk_decoder,
            mk_lb: self.mk_lb,

            multiplex,
        }
    }
}

#[derive(Clone)]
pub struct MessageService<Resp, MkE, MkD>
where
    Resp: EntryMessage + Send + 'static,
    MkE: MkEncoder + 'static,
    MkD: MkDecoder + 'static,
{
    #[cfg(not(feature = "multiplex"))]
    inner: pingpong::Client<Resp, MkE, MkD>,
    #[cfg(feature = "multiplex")]
    inner: motore::utils::Either<
        pingpong::Client<Resp, MkE, MkD>,
        crate::transport::multiplex::Client<Resp, MkE, MkD>,
    >,
}

impl<Req, Resp, MkE, MkD> Service<ClientContext, Req> for MessageService<Resp, MkE, MkD>
where
    MkE: MkEncoder + 'static,
    MkD: MkDecoder + 'static,
    Req: EntryMessage + 'static + Send,
    Resp: Send + 'static + EntryMessage,
{
    type Response = Option<Resp>;

    type Error = Error;

    type Future<'cx> = impl Future<Output = Result<Self::Response, Self::Error>> + 'cx + Send where Self:'cx;

    fn call<'cx, 's>(&'s mut self, cx: &'cx mut ClientContext, req: Req) -> Self::Future<'cx>
    where
        's: 'cx,
    {
        async move {
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
}

impl<IL, OL, C, Req, Resp, MkE: MkEncoder + 'static, MkD: MkDecoder + 'static, LB>
    ClientBuilder<IL, OL, C, Req, Resp, MkE, MkD, LB>
where
    C: SetClient<Req, Resp>,
    LB: MkLbLayer,
    LB::Layer: Layer<IL::Service>,
    <LB::Layer as Layer<IL::Service>>::Service:
        Service<ClientContext, Req, Response = Option<Resp>> + 'static + Send + Clone,
    <<LB::Layer as Layer<IL::Service>>::Service as Service<ClientContext, Req>>::Error:
        Into<crate::Error>,
    Req: EntryMessage + Send + 'static + Sync + Clone,
    Resp: EntryMessage + Send + 'static,
    IL: Layer<MessageService<Resp, MkE, MkD>>,
    IL::Service:
        Service<ClientContext, Req, Response = Option<Resp>> + Sync + Clone + Send + 'static,
    <IL::Service as Service<ClientContext, Req>>::Error: Send + Into<crate::Error>,
    MkD: MkDecoder + 'static,
    OL: Layer<
        BoxCloneService<
            ClientContext,
            Req,
            Option<Resp>,
            <<LB::Layer as Layer<IL::Service>>::Service as Service<ClientContext, Req>>::Error,
        >,
    >,
    OL::Service: Service<ClientContext, Req, Response = Option<Resp>> + 'static + Send + Clone,
    <OL::Service as Service<ClientContext, Req>>::Error: Send + Sync + Into<crate::Error>,
{
    /// Build volo client.
    pub fn build(self) -> C {
        let mc_cfg = volo::net::dial::Config::new(
            self.config.connect_timeout(),
            self.config.read_write_timeout(),
            self.config.read_write_timeout(),
        );
        let msg_svc = MessageService {
            #[cfg(not(feature = "multiplex"))]
            inner: pingpong::Client::new(
                MakeConnection::new(Some(mc_cfg)),
                self.codec_type,
                self.pool,
                self.mk_encoder,
                self.mk_decoder,
            ),
            #[cfg(feature = "multiplex")]
            inner: if !self.multiplex {
                motore::utils::Either::A(pingpong::Client::new(
                    MakeConnection::new(Some(mc_cfg)),
                    self.codec_type,
                    self.pool,
                    self.mk_encoder,
                    self.mk_decoder,
                ))
            } else {
                motore::utils::Either::B(crate::transport::multiplex::Client::new(
                    MakeConnection::new(Some(mc_cfg)),
                    self.codec_type,
                    self.pool,
                    self.mk_encoder,
                    self.mk_decoder,
                ))
            },
        };

        let transport = TimeoutLayer::new().layer(self.outer_layer.layer(BoxCloneService::new(
            self.mk_lb.make().layer(self.inner_layer.layer(msg_svc)),
        )));

        let transport = BoxCloneService::new(transport);

        self.service_client.set_client(Client {
            inner: Arc::new(ClientInner {
                callee_name: self.callee_name,
                config: self.config,
                address: self.address,
                caller_name: self.caller_name,
                seq_id: AtomicI32::new(0),
            }),
            callopt: None,
            transport,
        })
    }
}

/// A client for a Thrift service.
///
/// `Client` is designed to "clone and use", so it's cheap to clone it.
/// One important thing is that the `CallOpt` will not be cloned, because
/// it's designed to be per-request.
pub struct Client<Req, Resp> {
    transport: BoxCloneService<ClientContext, Req, Option<Resp>, crate::Error>,
    callopt: Option<CallOpt>,
    inner: Arc<ClientInner>,
}

unsafe impl<Req, Resp> Sync for Client<Req, Resp> {}

impl<Req, Resp> Clone for Client<Req, Resp> {
    fn clone(&self) -> Self {
        Self {
            transport: self.transport.clone(),
            callopt: None,
            inner: self.inner.clone(),
        }
    }
}

impl<Req, Resp> Client<Req, Resp> {
    #[inline]
    pub fn set_callopt(&mut self, callopt: CallOpt) {
        self.callopt = Some(callopt);
    }
}

struct ClientInner {
    callee_name: smol_str::SmolStr,
    caller_name: smol_str::SmolStr,
    config: Config,
    address: Option<Address>,
    seq_id: AtomicI32,
}

impl<Req, Resp> Client<Req, Resp> {
    pub async fn call(
        &mut self,
        method: &'static str,
        req: Req,
        oneway: bool,
    ) -> Result<Option<Resp>, Error> {
        let mut cx = ClientContext::new(
            self.inner
                .seq_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            self.make_rpc_info(method),
            if oneway {
                TMessageType::OneWay
            } else {
                TMessageType::Call
            },
        );

        cx.extensions_mut().insert(TransportType::TRANSPORT_FRAMED);

        let has_metainfo = metainfo::METAINFO.try_with(|_| {}).is_ok();

        let mk_call = async { self.transport.call(&mut cx, req).await };

        if has_metainfo {
            mk_call.await
        } else {
            metainfo::METAINFO
                .scope(RefCell::new(metainfo::MetaInfo::default()), mk_call)
                .await
        }
    }

    fn make_rpc_info(&mut self, method: &'static str) -> RpcInfo<Config> {
        let mut caller = Endpoint::new(self.inner.caller_name.clone());
        let mut callee = Endpoint::new(self.inner.callee_name.clone());
        if let Some(target) = &self.inner.address {
            callee.set_address(target.clone());
        }
        let mut config = self.inner.config;
        if let Some(co) = self.callopt.take() {
            callee.tags.extend(co.callee_tags);
            caller.tags.extend(co.caller_tags);
            if let Some(a) = co.address {
                callee.set_address(a);
            }
            config.merge(co.config);
        }

        RpcInfo::new(Role::Client, method.into(), caller, callee, config)
    }
}
