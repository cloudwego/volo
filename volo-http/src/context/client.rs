use chrono::{DateTime, Local};
use http::StatusCode;
use paste::paste;
use volo::{
    context::{Context, Reusable, Role, RpcCx, RpcInfo},
    net::Address,
    newtype_impl_context,
};

use super::CommonStats;

#[derive(Debug)]
pub struct ClientContext(pub(crate) RpcCx<ClientCxInner, Config>);

impl ClientContext {
    pub(crate) fn new(target: Address) -> Self {
        let mut cx = RpcCx::new(
            RpcInfo::with_role(Role::Client),
            ClientCxInner {
                stats: ClientStats::default(),
                common_stats: CommonStats::default(),
            },
        );
        cx.rpc_info_mut().callee_mut().set_address(target);
        Self(cx)
    }
}

newtype_impl_context!(ClientContext, Config, 0);

impl_deref_and_deref_mut!(ClientContext, RpcCx<ClientCxInner, Config>, 0);

#[derive(Debug)]
pub struct ClientCxInner {
    /// This is unstable now and may be changed in the future.
    pub stats: ClientStats,
    /// This is unstable now and may be changed in the future.
    pub common_stats: CommonStats,
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

    #[inline]
    pub fn status_code(&self) -> Option<StatusCode> {
        self.status_code
    }

    #[inline]
    pub fn set_status_code(&mut self, status: StatusCode) {
        self.status_code = Some(status);
    }
}

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub(crate) host: Host,
}

#[derive(Clone, Debug, Default)]
pub enum UserAgent {
    #[default]
    PkgNameWithVersion,
    CallerNameWithVersion,
    Specified(String),
    None,
}

#[derive(Clone, Debug, Default)]
pub enum Host {
    #[default]
    CalleeName,
    TargetAddress,
    None,
}

impl Reusable for Config {
    fn clear(&mut self) {
        *self = Default::default()
    }
}
