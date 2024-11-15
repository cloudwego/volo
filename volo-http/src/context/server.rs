//! Context and its utilities of server

use std::borrow::Cow;
use std::str::FromStr;
use faststr::FastStr;
use volo::{
    context::{Context, Reusable, Role, RpcCx, RpcInfo},
    net::Address,
    newtype_impl_context,
};

use crate::{
    server::param::PathParamsVec,
    utils::macros::{impl_deref_and_deref_mut, impl_getter},
};

/// RPC context of http server
#[derive(Debug)]
pub struct ServerContext(pub(crate) RpcCx<ServerCxInner, Config>);

impl ServerContext {
    /// Create a new [`ServerContext`] with the address of client
    pub fn new(peer: Address) -> Self {
        let mut cx = RpcCx::new(
            RpcInfo::<Config>::with_role(Role::Server),
            ServerCxInner::default(),
        );
        cx.rpc_info_mut().caller_mut().set_address(peer);
        Self(cx)
    }
}

impl_deref_and_deref_mut!(ServerContext, RpcCx<ServerCxInner, Config>, 0);

newtype_impl_context!(ServerContext, Config, 0);

/// Inner details of [`ServerContext`]
#[derive(Clone, Debug, Default)]
pub struct ServerCxInner {
    /// Path params from [`Uri`]
    ///
    /// See [`Router::route`] and [`PathParamsVec`], [`PathParamsMap`] or [`PathParams`] for more
    /// details.
    ///
    /// [`Uri`]: http::uri::Uri
    /// [`Router::route`]: crate::server::route::Router::route
    /// [`PathParamsMap`]: crate::server::param::PathParamsMap
    /// [`PathParams`]: crate::server::param::PathParams
    pub params: PathParamsVec,
}

impl ServerCxInner {
    impl_getter!(params, PathParamsVec);
}

/// Configuration of the request
///
/// It is empty currently
#[derive(Clone, Debug)]
pub struct Config {
    #[cfg(feature = "__tls")]
    tls: bool,

    // client ip
    remote_ip_headers: Vec<FastStr>,
    trusted_cidrs: Vec<ipnet::IpNet>,
}

impl Config {
    /// Return if the request is using TLS.
    #[cfg(feature = "__tls")]
    pub fn is_tls(&self) -> bool {
        self.tls
    }

    #[cfg(feature = "__tls")]
    pub(crate) fn set_tls(&mut self, tls: bool) {
        self.tls = tls;
    }

    pub fn set_remote_ip_headers<I>(&mut self, headers: I)
    where
        I: IntoIterator,
        I::Item: Into<Cow<'static, str>>,
    {
        let remote_ip_headers = headers.into_iter().map(Into::into)
            .map(|s|
                match s {
                    Cow::Owned(s) => FastStr::from_str(&s).unwrap(),
                    Cow::Borrowed(s) => FastStr::from_str(s).unwrap(),
                }
            )
            .collect();
        self.remote_ip_headers = remote_ip_headers;
    }

    pub(crate) fn remote_ip_headers(&self) -> &Vec<FastStr> {
        &self.remote_ip_headers
    }

    pub fn set_trusted_cidrs<H>(&mut self, cidrs: H)
    where
        H: IntoIterator<Item=ipnet::IpNet>,
    {
        self.trusted_cidrs = cidrs.into_iter().collect();
    }

    pub(crate) fn trusted_cidrs(&self) -> &Vec<ipnet::IpNet> {
        &self.trusted_cidrs
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            #[cfg(feature = "__tls")]
            tls: false,
            remote_ip_headers: vec!["X-Forwarded-For".into(), "X-Real-IP".into()],
            trusted_cidrs: vec!["0.0.0.0/0".parse().unwrap(), "::/0".parse().unwrap()],
        }
    }
}

impl Reusable for Config {
    fn clear(&mut self) {
        *self = Default::default();
    }
}
