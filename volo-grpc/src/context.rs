use std::time::Duration;

pub use volo::context::*;
use volo::newtype_impl_context;

use crate::codec::compression::CompressionEncoding;

pub struct ClientCxInner;

/// A context for client to pass information such as `RpcInfo` and `Config` between middleware
/// during the rpc call lifecycle.
pub struct ClientContext(pub(crate) RpcCx<ClientCxInner, Config>);

newtype_impl_context!(ClientContext, Config, 0);

impl ClientContext {
    pub fn new(ri: RpcInfo<Config>) -> Self {
        Self(RpcCx::new(ri, ClientCxInner))
    }
}

impl Default for ClientContext {
    fn default() -> Self {
        Self(RpcCx::new(RpcInfo::with_role(Role::Client), ClientCxInner))
    }
}

impl std::ops::Deref for ClientContext {
    type Target = RpcCx<ClientCxInner, Config>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ClientContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub struct ServerCxInner;

/// A context for server to pass information such as `RpcInfo` and `Config` between middleware
/// during the rpc call lifecycle.
pub struct ServerContext(pub(crate) RpcCx<ServerCxInner, Config>);

newtype_impl_context!(ServerContext, Config, 0);

impl Default for ServerContext {
    fn default() -> Self {
        Self(RpcCx::new(RpcInfo::with_role(Role::Server), ServerCxInner))
    }
}

impl std::ops::Deref for ServerContext {
    type Target = RpcCx<ServerCxInner, Config>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ServerContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Default, Debug, Clone)]
pub struct Config {
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
}
