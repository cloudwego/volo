//! Context and its utilities of server
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};
use volo::{
    context::{Context, Reusable, Role, RpcCx, RpcInfo},
    net::Address,
    newtype_impl_context,
};

use crate::{
    server::param::PathParamsVec,
    utils::macros::{impl_deref_and_deref_mut, impl_getter, stat_impl},
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

    /// Statistics of the request
    pub stats: Arc<Mutex<ServerStats>>,
}

impl ServerCxInner {
    impl_getter!(params, PathParamsVec);

    /// Return the statistics of the request
    pub fn stats(&self) -> &Arc<Mutex<ServerStats>> {
        &self.stats
    }
}

/// Statistics of server
#[derive(Debug, Default, Clone)]
pub struct ServerStats {
    read_header_start: Option<DateTime<Local>>,
    read_header_finish: Option<DateTime<Local>>,
    read_body_start: Option<DateTime<Local>>,
    read_body_finish: Option<DateTime<Local>>,
    handle_start: Option<DateTime<Local>>,
    handle_finish: Option<DateTime<Local>>,
    write_start: Option<DateTime<Local>>,
    write_finish: Option<DateTime<Local>>,
}

impl ServerStats {
    stat_impl!(read_header_start);
    stat_impl!(read_header_finish);
    stat_impl!(read_body_start);
    stat_impl!(read_body_finish);
    stat_impl!(handle_start);
    stat_impl!(handle_finish);
    stat_impl!(write_start);
    stat_impl!(write_finish);
}

/// Configuration of the request
///
/// It is empty currently
#[derive(Clone, Debug, Default)]
pub struct Config {
    #[cfg(feature = "__tls")]
    tls: bool,
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
}

impl Reusable for Config {
    fn clear(&mut self) {
        *self = Default::default();
    }
}
