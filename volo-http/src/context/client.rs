use chrono::{DateTime, Local};
use faststr::FastStr;
use http::StatusCode;
use paste::paste;
use volo::{
    context::{Context, Reusable, Role, RpcCx, RpcInfo},
    net::Address,
    newtype_impl_context,
};

use super::CommonStats;
use crate::utils::macros::impl_deref_and_deref_mut;

#[derive(Debug)]
pub struct ClientContext(pub(crate) RpcCx<ClientCxInner, Config>);

impl ClientContext {
    pub fn new(target: Address, #[cfg(feature = "__tls")] tls: bool) -> Self {
        let mut cx = RpcCx::new(
            RpcInfo::<Config>::with_role(Role::Client),
            ClientCxInner {
                #[cfg(feature = "__tls")]
                tls,
                stats: ClientStats::default(),
                common_stats: CommonStats::default(),
            },
        );
        cx.rpc_info_mut().callee_mut().set_address(target);
        Self(cx)
    }

    pub fn enable_stat(&mut self, stat: bool) {
        self.rpc_info_mut().config_mut().stat_enable = stat;
    }

    pub(crate) fn stat_enabled(&self) -> bool {
        self.rpc_info().config().stat_enable
    }
}

newtype_impl_context!(ClientContext, Config, 0);

impl_deref_and_deref_mut!(ClientContext, RpcCx<ClientCxInner, Config>, 0);

#[derive(Debug)]
pub struct ClientCxInner {
    #[cfg(feature = "__tls")]
    tls: bool,

    /// This is unstable now and may be changed in the future.
    pub stats: ClientStats,
    /// This is unstable now and may be changed in the future.
    pub common_stats: CommonStats,
}

impl ClientCxInner {
    #[cfg(feature = "__tls")]
    pub fn is_tls(&self) -> bool {
        self.tls
    }
}

/// This is unstable now and may be changed in the future.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClientStats {
    transport_start_at: Option<DateTime<Local>>,
    transport_end_at: Option<DateTime<Local>>,

    status_code: Option<StatusCode>,
}

impl ClientStats {
    stat_impl!(transport_start_at);
    stat_impl!(transport_end_at);
    stat_impl_getter_and_setter!(status_code, StatusCode);
}

#[derive(Clone, Debug)]
pub struct Config {
    pub(crate) caller_name: CallerName,
    pub(crate) callee_name: CalleeName,
    pub(crate) stat_enable: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            caller_name: CallerName::default(),
            callee_name: CalleeName::default(),
            stat_enable: true,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum CallerName {
    /// The crate name and version of the current crate.
    #[default]
    PkgNameWithVersion,
    /// The original caller name.
    OriginalCallerName,
    /// The caller name and version of the current crate.
    CallerNameWithVersion,
    /// A specified String for the user agent.
    Specified(FastStr),
    /// Do not set `User-Agent` by the client.
    None,
}

#[derive(Clone, Debug, Default)]
pub enum CalleeName {
    /// The target authority of URI.
    #[default]
    TargetName,
    /// The original callee name.
    OriginalCalleeName,
    /// Do not set `Host` by the client.
    None,
}

impl Reusable for Config {
    fn clear(&mut self) {
        *self = Default::default()
    }
}
