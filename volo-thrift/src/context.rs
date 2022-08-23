use std::time::Duration;

use pilota::thrift::TMessageIdentifier;
use volo::{
    context::{Role, RpcCx, RpcInfo},
    newtype_impl_context,
};

use crate::protocol::TMessageType;

#[derive(Default, Clone, Debug)]
pub struct ServerTransportInfo {
    conn_reset: bool,
}

impl ServerTransportInfo {
    pub fn is_conn_reset(&self) -> bool {
        self.conn_reset
    }

    pub fn set_conn_reset(&mut self, reset: bool) {
        self.conn_reset = reset
    }

    pub fn reset(&mut self) {
        *self = Self { ..Self::default() }
    }
}

#[derive(Default)]
pub struct PooledTransport {
    pub should_reuse: bool,
}

impl PooledTransport {
    pub fn set_reuse(&mut self, should_reuse: bool) {
        if !self.should_reuse && should_reuse {
            panic!("cannot reuse a transport which should_reuse is false");
        }
        self.should_reuse = should_reuse;
    }
}

pub struct ClientCxInner {
    pub seq_id: i32,
    pub message_type: TMessageType,
    pub transport: PooledTransport,
}

pub struct ServerCxInner {
    pub seq_id: Option<i32>,
    pub req_msg_type: Option<TMessageType>,
    pub msg_type: Option<TMessageType>,
    pub transport: ServerTransportInfo,
}

pub struct ClientContext(pub(crate) volo::context::RpcCx<ClientCxInner, Config>);

newtype_impl_context!(ClientContext, Config, 0);

impl ClientContext {
    pub fn new(seq_id: i32, ri: RpcInfo<Config>, msg_type: TMessageType) -> Self {
        Self(RpcCx::new(
            ri,
            ClientCxInner {
                seq_id,
                message_type: msg_type,
                transport: PooledTransport { should_reuse: true },
            },
        ))
    }
}

impl std::ops::Deref for ClientContext {
    type Target = volo::context::RpcCx<ClientCxInner, Config>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ClientContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub struct ServerContext(pub(crate) volo::context::RpcCx<ServerCxInner, Config>);

impl Default for ServerContext {
    fn default() -> Self {
        Self(RpcCx::new(
            RpcInfo::with_role(Role::Server),
            ServerCxInner {
                seq_id: None,
                req_msg_type: None,
                msg_type: None,
                transport: ServerTransportInfo::default(),
            },
        ))
    }
}

newtype_impl_context!(ServerContext, Config, 0);

impl std::ops::Deref for ServerContext {
    type Target = volo::context::RpcCx<ServerCxInner, Config>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ServerContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub trait ThriftContext: volo::context::Context<Config = Config> + Send + 'static {
    fn encode_conn_reset(&self) -> Option<bool>;
    fn set_conn_reset_by_ttheader(&mut self, reset: bool);
    fn handle_decoded_msg_ident(&mut self, ident: &TMessageIdentifier);
    fn seq_id(&self) -> i32;
    fn msg_type(&self) -> TMessageType;
}

impl ThriftContext for ClientContext {
    #[inline]
    fn encode_conn_reset(&self) -> Option<bool> {
        None
    }

    #[inline]
    fn set_conn_reset_by_ttheader(&mut self, reset: bool) {
        self.transport.set_reuse(!reset);
    }

    #[inline]
    fn handle_decoded_msg_ident(&mut self, _ident: &TMessageIdentifier) {}

    #[inline]
    fn seq_id(&self) -> i32 {
        self.seq_id
    }

    #[inline]
    fn msg_type(&self) -> TMessageType {
        self.message_type
    }
}

impl ThriftContext for ServerContext {
    #[inline]
    fn encode_conn_reset(&self) -> Option<bool> {
        Some(self.transport.is_conn_reset())
    }

    #[inline]
    fn set_conn_reset_by_ttheader(&mut self, _reset: bool) {}

    #[inline]
    fn handle_decoded_msg_ident(&mut self, ident: &TMessageIdentifier) {
        self.seq_id = Some(ident.sequence_number);
        self.req_msg_type = Some(ident.message_type);
        self.rpc_info.method = Some(ident.name.clone());
    }

    #[inline]
    fn seq_id(&self) -> i32 {
        self.seq_id.unwrap_or(0)
    }

    #[inline]
    fn msg_type(&self) -> TMessageType {
        self.msg_type.unwrap()
    }
}

// defaults to 16M
const DEFAULT_MAX_FRAME_SIZE: u32 = 1024 * 1024 * 16;
const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(1);
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_millis(50);
const DEFAULT_READ_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy)]
pub struct Config {
    rpc_timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    read_write_timeout: Option<Duration>,
    max_frame_size: u32,
}

impl Config {
    pub fn new() -> Self {
        Self {
            rpc_timeout: Some(DEFAULT_RPC_TIMEOUT),
            connect_timeout: None,
            read_write_timeout: None,
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
        }
    }

    pub fn rpc_timeout(&self) -> Option<Duration> {
        self.rpc_timeout
    }

    /// Sets the rpc timeout.
    ///
    /// This can be set both by the client builder and the CallOpt.
    pub fn set_rpc_timeout(&mut self, rpc_timeout: Option<Duration>) {
        self.rpc_timeout = rpc_timeout;
    }

    pub fn rpc_timeout_or_default(&self) -> Duration {
        self.rpc_timeout.unwrap_or(DEFAULT_RPC_TIMEOUT)
    }

    pub fn connect_timeout(&self) -> Option<Duration> {
        self.connect_timeout
    }

    pub fn connect_timeout_or_default(&self) -> Duration {
        self.connect_timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT)
    }

    /// Sets the connect timeout.
    pub(crate) fn set_connect_timeout(&mut self, timeout: Option<Duration>) {
        self.connect_timeout = timeout;
    }

    pub fn read_write_timeout(&self) -> Option<Duration> {
        self.read_write_timeout
    }

    pub fn read_write_timeout_or_default(&self) -> Duration {
        self.read_write_timeout
            .unwrap_or(DEFAULT_READ_WRITE_TIMEOUT)
    }

    /// Sets the read write timeout(a.k.a. IO timeout).
    pub(crate) fn set_read_write_timeout(&mut self, timeout: Option<Duration>) {
        self.read_write_timeout = timeout;
    }

    pub fn max_frame_size(&self) -> u32 {
        self.max_frame_size
    }

    pub(crate) fn set_max_frame_size(&mut self, size: u32) {
        self.max_frame_size = size
    }

    pub fn merge(&mut self, other: Self) {
        self.max_frame_size = other.max_frame_size;
        if let Some(t) = other.rpc_timeout {
            self.rpc_timeout = Some(t);
        }
        if let Some(t) = other.connect_timeout {
            self.connect_timeout = Some(t);
        }
        if let Some(t) = other.read_write_timeout {
            self.read_write_timeout = Some(t);
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            rpc_timeout: None,
            connect_timeout: None,
            read_write_timeout: None,
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
        }
    }
}
