use std::fmt::Debug;

pub use metainfo::MetaInfo;
use metainfo::{FastStrMap, TypeMap};

use super::net::Address;
use crate::FastStr;

#[macro_export]
macro_rules! newtype_impl_context {
    ($t:ident, $cf:ident, $inner: tt) => {
        impl $crate::context::Context for $t {
            type Config = $cf;

            #[inline]
            fn rpc_info(&self) -> &$crate::context::RpcInfo<Self::Config> {
                self.$inner.rpc_info()
            }

            #[inline]
            fn rpc_info_mut(&mut self) -> &mut $crate::context::RpcInfo<Self::Config> {
                self.$inner.rpc_info_mut()
            }

            #[inline]
            fn extensions_mut(&mut self) -> &mut $crate::context::Extensions {
                self.$inner.extensions_mut()
            }

            #[inline]
            fn extensions(&self) -> &$crate::context::Extensions {
                self.$inner.extensions()
            }
        }
    };
}

const DEFAULT_MAP_CAPACITY: usize = 10;

pub trait Reusable {
    fn clear(&mut self);
}

#[derive(Debug)]
pub struct RpcCx<I, Config: Reusable + Default> {
    pub rpc_info: RpcInfo<Config>,
    pub inner: I,
    pub extensions: Extensions,
}

#[derive(Default, Debug)]
pub struct Extensions(TypeMap);

impl std::ops::Deref for Extensions {
    type Target = TypeMap;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Extensions {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub trait Context {
    type Config: Reusable + Default + Send + Debug;

    fn rpc_info(&self) -> &RpcInfo<Self::Config>;
    fn rpc_info_mut(&mut self) -> &mut RpcInfo<Self::Config>;

    fn extensions(&self) -> &Extensions;
    fn extensions_mut(&mut self) -> &mut Extensions;
}

impl<I, Config> Context for RpcCx<I, Config>
where
    Config: Reusable + Default + Send + Debug,
{
    type Config = Config;

    #[inline]
    fn rpc_info(&self) -> &RpcInfo<Self::Config> {
        &self.rpc_info
    }

    #[inline]
    fn rpc_info_mut(&mut self) -> &mut RpcInfo<Self::Config> {
        &mut self.rpc_info
    }

    #[inline]
    fn extensions(&self) -> &Extensions {
        &self.extensions
    }

    #[inline]
    fn extensions_mut(&mut self) -> &mut Extensions {
        &mut self.extensions
    }
}

impl<I, Config> std::ops::Deref for RpcCx<I, Config>
where
    Config: Reusable + Default,
{
    type Target = I;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<I, Config> std::ops::DerefMut for RpcCx<I, Config>
where
    Config: Reusable + Default,
{
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<I, Config> RpcCx<I, Config>
where
    Config: Reusable + Default,
{
    #[inline]
    pub fn new(ri: RpcInfo<Config>, inner: I) -> Self {
        Self {
            rpc_info: ri,
            inner,
            extensions: Extensions(TypeMap::with_capacity(DEFAULT_MAP_CAPACITY)),
        }
    }

    #[inline]
    pub fn reset(&mut self, inner: I) {
        self.rpc_info.clear();
        self.inner = inner;
        self.extensions.clear();
    }
}

/// Endpoint contains the information of the service.
#[derive(Debug, Default)]
pub struct Endpoint {
    /// `service_name` is the most important information, which is used by the service discovering.
    pub service_name: FastStr,
    pub address: Option<Address>,
    pub shmipc_address: Option<Address>,
    /// `faststr_tags` is a optimized typemap to store additional information of the endpoint.
    ///
    /// Use `FastStrMap` instead of `TypeMap` can reduce the Box allocation.
    ///
    /// This is mainly for performance optimization.
    pub faststr_tags: FastStrMap,
    /// `tags` is used to store additional information of the endpoint.
    ///
    /// Users can use `tags` to store custom data, such as the datacenter name or the region name,
    /// which can be used by the service discoverer.
    pub tags: TypeMap,
}

impl Endpoint {
    /// Creates a new endpoint info.
    #[inline]
    pub fn new(service_name: FastStr) -> Self {
        Self {
            service_name,
            address: None,
            shmipc_address: None,
            faststr_tags: FastStrMap::with_capacity(DEFAULT_MAP_CAPACITY),
            tags: Default::default(),
        }
    }

    /// Gets the service name of the endpoint.
    #[inline]
    pub fn service_name_ref(&self) -> &str {
        &self.service_name
    }

    #[inline]
    pub fn service_name(&self) -> FastStr {
        self.service_name.clone()
    }

    #[inline]
    pub fn set_service_name(&mut self, service_name: FastStr) {
        self.service_name = service_name;
    }

    /// Insert a tag into this `Endpoint`.
    #[inline]
    pub fn insert<T: Send + Sync + 'static>(&mut self, val: T) {
        self.tags.insert(val);
    }

    /// Check if `Endpoint` tags contain entry
    #[inline]
    pub fn contains<T: 'static>(&self) -> bool {
        self.tags.contains::<T>()
    }

    /// Get a reference to a tag previously inserted on this `Endpoint`.
    #[inline]
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.tags.get::<T>()
    }

    /// Insert a tag into this `Endpoint`.
    #[inline]
    pub fn insert_faststr<T: Send + Sync + 'static>(&mut self, val: FastStr) {
        self.faststr_tags.insert::<T>(val);
    }

    /// Check if `Endpoint` tags contain entry
    #[inline]
    pub fn contains_faststr<T: 'static>(&self) -> bool {
        self.faststr_tags.contains::<T>()
    }

    /// Get a reference to a tag previously inserted on this `Endpoint`.
    #[inline]
    pub fn get_faststr<T: 'static>(&self) -> Option<&FastStr> {
        self.faststr_tags.get::<T>()
    }

    /// Sets the address.
    #[inline]
    pub fn set_address(&mut self, address: Address) {
        self.address = Some(address)
    }

    /// Gets the address.
    #[inline]
    pub fn address(&self) -> Option<Address> {
        self.address.clone()
    }

    /// Sets the shmipc address.
    #[inline]
    pub fn set_shmipc_address(&mut self, shmipc_address: Address) {
        self.shmipc_address = Some(shmipc_address)
    }

    /// Gets the shmipc address.
    #[inline]
    pub fn shmipc_address(&self) -> Option<Address> {
        self.shmipc_address.clone()
    }

    /// Clear the information
    #[inline]
    pub fn clear(&mut self) {
        self.service_name = FastStr::from_static_str("");
        self.address = None;
        self.faststr_tags.clear();
        self.tags.clear();
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Role {
    Client,
    Server,
}

#[derive(Debug)]
pub struct RpcInfo<Config: Reusable + Default> {
    role: Role,
    caller: Endpoint,
    callee: Endpoint,
    method: FastStr,
    config: Config,
}

impl<Config: Reusable + Default> RpcInfo<Config> {
    #[inline]
    pub fn with_role(role: Role) -> RpcInfo<Config> {
        RpcInfo {
            role,
            caller: Default::default(),
            callee: Default::default(),
            method: Default::default(),
            config: Default::default(),
        }
    }

    #[inline]
    pub fn new(
        role: Role,
        method: FastStr,
        caller: Endpoint,
        callee: Endpoint,
        config: Config,
    ) -> Self {
        RpcInfo {
            role,
            caller,
            callee,
            method,
            config,
        }
    }

    #[inline]
    pub fn role(&self) -> Role {
        self.role
    }

    #[inline]
    pub fn set_role(&mut self, role: Role) {
        self.role = role;
    }

    #[inline]
    pub fn method(&self) -> &FastStr {
        &self.method
    }

    #[inline]
    pub fn method_mut(&mut self) -> &mut FastStr {
        &mut self.method
    }

    #[inline]
    pub fn set_method(&mut self, method: FastStr) {
        self.method = method;
    }

    #[inline]
    pub fn caller(&self) -> &Endpoint {
        &self.caller
    }

    #[inline]
    pub fn caller_mut(&mut self) -> &mut Endpoint {
        &mut self.caller
    }

    #[inline]
    pub fn callee(&self) -> &Endpoint {
        &self.callee
    }

    #[inline]
    pub fn callee_mut(&mut self) -> &mut Endpoint {
        &mut self.callee
    }

    #[inline]
    pub fn config(&self) -> &Config {
        &self.config
    }

    #[inline]
    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }

    #[inline]
    pub fn set_config(&mut self, config: Config) {
        self.config = config;
    }

    #[inline]
    pub fn clear(&mut self) {
        self.caller.clear();
        self.callee.clear();
        self.method = Default::default();
        self.config.clear();
    }
}
