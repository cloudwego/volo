use std::time::Duration;

use chrono::{DateTime, Local};
use volo::{
    context::{Reusable, Role, RpcCx, RpcInfo},
    newtype_impl_context,
};

use crate::utils::macros::{impl_deref_and_deref_mut, stat_impl};

#[derive(Debug)]
pub struct ClientContext(pub(crate) RpcCx<ClientCxInner, Config>);

impl ClientContext {
    pub fn new() -> Self {
        Self(RpcCx::new(
            RpcInfo::<Config>::with_role(Role::Client),
            ClientCxInner {
                stats: ClientStats::default(),
            },
        ))
    }
}

impl Default for ClientContext {
    fn default() -> Self {
        Self::new()
    }
}

newtype_impl_context!(ClientContext, Config, 0);

impl_deref_and_deref_mut!(ClientContext, RpcCx<ClientCxInner, Config>, 0);

#[derive(Debug)]
pub struct ClientCxInner {
    /// This is unstable now and may be changed in the future.
    pub stats: ClientStats,
}

/// This is unstable now and may be changed in the future.
#[derive(Debug, Default, Clone)]
pub struct ClientStats {
    transport_start_at: Option<DateTime<Local>>,
    transport_end_at: Option<DateTime<Local>>,
}

impl ClientStats {
    stat_impl!(transport_start_at);
    stat_impl!(transport_end_at);
}

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub timeout: Option<Duration>,
}

impl Reusable for Config {
    fn clear(&mut self) {
        *self = Default::default()
    }
}
