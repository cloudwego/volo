use std::time::Duration;

use chrono::{DateTime, Local};
use paste::paste;
pub use volo::context::*;
use volo::newtype_impl_context;

use crate::codec::compression::CompressionEncoding;

macro_rules! stat_impl {
    ($t: ident) => {
        paste! {
            #[inline]
            pub fn $t(&self) -> Option<DateTime<Local>> {
                self.$t
            }

            #[doc(hidden)]
            #[inline]
            pub fn [<set_$t>](&mut self, t: DateTime<Local>) {
                self.$t = Some(t)
            }

            #[inline]
            pub fn [<record_ $t>](&mut self) {
                self.$t = Some(Local::now())
            }
        }
    };
}

#[derive(Debug, Default, Clone)]
pub struct CommonStats {
    read_start_at: Option<DateTime<Local>>,
    read_end_at: Option<DateTime<Local>>,

    decode_start_at: Option<DateTime<Local>>,
    decode_end_at: Option<DateTime<Local>>,
    encode_start_at: Option<DateTime<Local>>,
    encode_end_at: Option<DateTime<Local>>,
    write_start_at: Option<DateTime<Local>>,
    write_end_at: Option<DateTime<Local>>,
}

impl CommonStats {
    stat_impl!(read_start_at);
    stat_impl!(read_end_at);
    stat_impl!(decode_start_at);
    stat_impl!(decode_end_at);
    stat_impl!(encode_start_at);
    stat_impl!(encode_end_at);
    stat_impl!(write_start_at);
    stat_impl!(write_end_at);

    #[inline]
    pub fn reset(&mut self) {
        *self = Self { ..Self::default() }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ClientStats {
    make_transport_start_at: Option<DateTime<Local>>,
    make_transport_end_at: Option<DateTime<Local>>,
}

impl ClientStats {
    stat_impl!(make_transport_start_at);
    stat_impl!(make_transport_end_at);

    #[inline]
    pub fn reset(&mut self) {
        self.make_transport_start_at = None;
        self.make_transport_end_at = None;
    }
}

#[derive(Debug, Default, Clone)]
pub struct ServerStats {
    process_start_at: Option<DateTime<Local>>,
    process_end_at: Option<DateTime<Local>>,
}

impl ServerStats {
    stat_impl!(process_start_at);
    stat_impl!(process_end_at);

    #[inline]
    pub fn reset(&mut self) {
        self.process_start_at = None;
        self.process_end_at = None;
    }
}

#[derive(Debug, Clone, Default)]
pub struct ClientCxInner {
    pub stats: ClientStats,
    pub common_stats: CommonStats,
}

/// A context for client to pass information such as `RpcInfo` and `Config` between middleware
/// during the rpc call lifecycle.
pub struct ClientContext(pub(crate) RpcCx<ClientCxInner, Config>);

newtype_impl_context!(ClientContext, Config, 0);

impl ClientContext {
    pub fn new(ri: RpcInfo<Config>) -> Self {
        Self(RpcCx::new(
            ri,
            ClientCxInner {
                stats: ClientStats::default(),
                common_stats: CommonStats::default(),
            },
        ))
    }
}

impl Default for ClientContext {
    fn default() -> Self {
        Self::new(RpcInfo::with_role(Role::Client))
    }
}

impl std::ops::Deref for ClientContext {
    type Target = RpcCx<ClientCxInner, Config>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ClientContext {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Debug, Clone, Default)]
pub struct ServerCxInner {
    pub stats: ServerStats,
    pub common_stats: CommonStats,
}

/// A context for server to pass information such as `RpcInfo` and `Config` between middleware
/// during the rpc call lifecycle.
pub struct ServerContext(pub(crate) RpcCx<ServerCxInner, Config>);

newtype_impl_context!(ServerContext, Config, 0);

impl Default for ServerContext {
    fn default() -> Self {
        Self(RpcCx::new(
            RpcInfo::with_role(Role::Server),
            Default::default(),
        ))
    }
}

impl std::ops::Deref for ServerContext {
    type Target = RpcCx<ServerCxInner, Config>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ServerContext {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub trait GrpcContext: volo::context::Context<Config = Config> + Send + 'static {
    #[doc(hidden)]
    fn stats(&self) -> &CommonStats;
    #[doc(hidden)]
    fn stats_mut(&mut self) -> &mut CommonStats;
}

impl GrpcContext for ClientContext {
    #[inline]
    fn stats(&self) -> &CommonStats {
        &self.common_stats
    }

    #[inline]
    fn stats_mut(&mut self) -> &mut CommonStats {
        &mut self.common_stats
    }
}

impl GrpcContext for ServerContext {
    #[inline]
    fn stats(&self) -> &CommonStats {
        &self.common_stats
    }

    #[inline]
    fn stats_mut(&mut self) -> &mut CommonStats {
        &mut self.common_stats
    }
}

const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Default, Debug, Clone)]
pub struct Config {
    pub(crate) rpc_timeout: Option<Duration>,
    /// Amount of time to wait connecting.
    pub(crate) connect_timeout: Option<Duration>,
    /// Amount of time to wait reading.
    pub(crate) read_timeout: Option<Duration>,
    /// Amount of time to wait reading response.
    pub(crate) write_timeout: Option<Duration>,

    pub(crate) accept_compressions: Option<Vec<CompressionEncoding>>,
    pub(crate) send_compressions: Option<Vec<CompressionEncoding>>,
}

impl Reusable for Config {
    fn clear(&mut self) {
        self.rpc_timeout = None;
        self.connect_timeout = None;
        self.read_timeout = None;
        self.write_timeout = None;
        if let Some(v) = self.accept_compressions.as_mut() {
            v.clear();
        }
        if let Some(v) = self.send_compressions.as_mut() {
            v.clear();
        }
    }
}

impl Config {
    pub fn merge(&mut self, other: Self) {
        if let Some(t) = other.rpc_timeout {
            self.rpc_timeout = Some(t);
        }
        if let Some(t) = other.connect_timeout {
            self.connect_timeout = Some(t);
        }
        if let Some(t) = other.read_timeout {
            self.read_timeout = Some(t);
        }
        if let Some(t) = other.write_timeout {
            self.write_timeout = Some(t);
        }
        if let Some(e) = other.accept_compressions {
            self.accept_compressions = Some(e);
        }
        if let Some(e) = other.send_compressions {
            self.send_compressions = Some(e);
        }
    }

    #[inline]
    pub fn rpc_timeout(&self) -> Option<Duration> {
        self.rpc_timeout
    }

    /// Sets the rpc timeout.
    ///
    /// This can be set both by the client builder and the CallOpt.
    #[inline]
    pub fn set_rpc_timeout(&mut self, rpc_timeout: Option<Duration>) {
        self.rpc_timeout = rpc_timeout;
    }

    #[inline]
    pub fn rpc_timeout_or_default(&self) -> Duration {
        self.rpc_timeout.unwrap_or(DEFAULT_RPC_TIMEOUT)
    }
}
