//! Context and its utilities of client

use std::time::Duration;

use chrono::{DateTime, Local};
use volo::{
    context::{Reusable, Role, RpcCx, RpcInfo},
    newtype_impl_context,
};

use crate::utils::macros::{impl_deref_and_deref_mut, stat_impl};

/// RPC context of http client
#[derive(Debug)]
pub struct ClientContext(pub(crate) RpcCx<ClientCxInner, Config>);

impl ClientContext {
    /// Create a new [`ClientContext`]
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

/// Inner details of [`ClientContext`]
#[derive(Debug)]
pub struct ClientCxInner {
    /// Statistics of client
    ///
    /// This is unstable now and may be changed in the future.
    pub stats: ClientStats,
}

/// Statistics of client
///
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

/// Configuration of the request
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// Timeout of the whole request
    pub timeout: Option<Duration>,
    /// Return `Err` if status code of response is 4XX or 5XX
    pub fail_on_error_status: bool,
}

impl Reusable for Config {
    fn clear(&mut self) {
        *self = Default::default()
    }
}
