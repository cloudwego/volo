use std::time::Duration;

use chrono::{DateTime, Local};
use faststr::FastStr;
use volo::{
    context::{Reusable, Role, RpcCx, RpcInfo},
    newtype_impl_context,
};

use crate::utils::macros::{impl_deref_and_deref_mut, stat_impl};

#[derive(Debug)]
pub struct ClientContext(pub(crate) RpcCx<ClientCxInner, Config>);

impl ClientContext {
    #[allow(clippy::new_without_default)]
    pub fn new(
        #[cfg(feature = "__tls")]
        #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
        tls: bool,
    ) -> Self {
        Self(RpcCx::new(
            RpcInfo::<Config>::with_role(Role::Client),
            ClientCxInner {
                #[cfg(feature = "__tls")]
                tls,
                stats: ClientStats::default(),
            },
        ))
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
}

impl ClientCxInner {
    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    pub fn is_tls(&self) -> bool {
        self.tls
    }
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
