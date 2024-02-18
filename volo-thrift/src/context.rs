use std::time::Duration;

use chrono::{DateTime, Local};
use paste::paste;
use pilota::thrift::TMessageIdentifier;
use volo::{
    context::{Context, Reusable, Role, RpcCx, RpcInfo},
    newtype_impl_context,
};

use crate::{client::CallOpt, protocol::TMessageType};

macro_rules! stat_impl {
    ($t: ident) => {
        paste! {
            /// This is unstable now and may be changed in the future.
            #[inline]
            pub fn $t(&self) -> Option<DateTime<Local>> {
                self.$t
            }

            /// This is unstable now and may be changed in the future.
            #[doc(hidden)]
            #[inline]
            pub fn [<set_$t>](&mut self, t: DateTime<Local>) {
                self.$t = Some(t)
            }

            /// This is unstable now and may be changed in the future.
            #[inline]
            pub fn [<record_ $t>](&mut self) {
                self.$t = Some(Local::now())
            }
        }
    };
}

#[derive(Default, Clone, Debug, Copy)]
pub struct ServerTransportInfo {
    conn_reset: bool,
}

impl ServerTransportInfo {
    #[inline]
    pub fn is_conn_reset(&self) -> bool {
        self.conn_reset
    }

    #[inline]
    pub fn set_conn_reset(&mut self, reset: bool) {
        self.conn_reset = reset
    }

    #[inline]
    pub fn reset(&mut self) {
        *self = Self { ..Self::default() }
    }
}

/// This is unstable now and may be changed in the future.
#[derive(Debug, Default, Clone, Copy)]
pub struct CommonStats {
    // if there's a length-prefixed transport, we can get the read time
    read_start_at: Option<DateTime<Local>>,
    read_end_at: Option<DateTime<Local>>,

    decode_start_at: Option<DateTime<Local>>,
    decode_end_at: Option<DateTime<Local>>,
    encode_start_at: Option<DateTime<Local>>,
    encode_end_at: Option<DateTime<Local>>,
    write_start_at: Option<DateTime<Local>>,
    write_end_at: Option<DateTime<Local>>,

    // size
    read_size: Option<usize>, /* only applicable to length-prefixed transport such as TTHeader
                               * and Framed */
    write_size: Option<usize>,
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
    pub fn read_size(&self) -> Option<usize> {
        self.read_size
    }

    #[inline]
    pub fn set_read_size(&mut self, size: usize) {
        self.read_size = Some(size)
    }

    #[inline]
    pub fn write_size(&self) -> Option<usize> {
        self.write_size
    }

    #[inline]
    pub fn set_write_size(&mut self, size: usize) {
        self.write_size = Some(size)
    }

    #[inline]
    pub fn reset(&mut self) {
        *self = Self { ..Self::default() }
    }
}

/// This is unstable now and may be changed in the future.
#[derive(Debug, Default, Clone, Copy)]
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

/// This is unstable now and may be changed in the future.
#[derive(Debug, Default, Clone, Copy)]
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

#[derive(Default, Clone, Debug, Copy)]
pub struct PooledTransport {
    pub should_reuse: bool,
}

impl PooledTransport {
    #[inline]
    pub fn set_reuse(&mut self, should_reuse: bool) {
        if !self.should_reuse && should_reuse {
            panic!("cannot reuse a transport which should_reuse is false");
        }
        self.should_reuse = should_reuse;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ClientCxInner {
    pub seq_id: i32,
    pub message_type: TMessageType,
    pub transport: PooledTransport,
    /// This is unstable now and may be changed in the future.
    pub stats: ClientStats,
    /// This is unstable now and may be changed in the future.
    pub common_stats: CommonStats,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ServerCxInner {
    pub seq_id: Option<i32>,
    pub req_msg_type: Option<TMessageType>,
    pub msg_type: Option<TMessageType>,
    pub transport: ServerTransportInfo,
    /// This is unstable now and may be changed in the future.
    pub stats: ServerStats,
    /// This is unstable now and may be changed in the future.
    pub common_stats: CommonStats,
}

#[derive(Debug)]
pub struct ClientContext(pub(crate) RpcCx<ClientCxInner, Config>);

newtype_impl_context!(ClientContext, Config, 0);

impl ClientContext {
    #[inline]
    pub fn new(seq_id: i32, ri: RpcInfo<Config>, msg_type: TMessageType) -> Self {
        Self(RpcCx::new(
            ri,
            ClientCxInner {
                seq_id,
                message_type: msg_type,
                transport: PooledTransport { should_reuse: true },
                stats: ClientStats::default(),
                common_stats: CommonStats::default(),
            },
        ))
    }

    #[inline]
    pub fn reset(&mut self, seq_id: i32, msg_type: TMessageType) {
        self.seq_id = seq_id;
        self.message_type = msg_type;
        self.transport.should_reuse = true;
        self.stats.reset();
        self.common_stats.reset();
        // self.0 is RpcCx, this reset will clear rpcinfo and extension
        self.0.reset(self.0.inner);
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

thread_local! {
    #[doc(hidden)]
    /// This should only be used in the generated code.
    pub static CLIENT_CONTEXT_CACHE: std::cell::RefCell<Vec<ClientContext>> = std::cell::RefCell::new(Vec::with_capacity(128));
}

thread_local! {
    pub(crate) static SERVER_CONTEXT_CACHE: std::cell::RefCell<Vec<ServerContext>> = std::cell::RefCell::new(Vec::with_capacity(128));
}

#[derive(Debug)]
pub struct ServerContext(pub(crate) RpcCx<ServerCxInner, Config>);

impl Default for ServerContext {
    #[inline]
    fn default() -> Self {
        Self(RpcCx::new(
            RpcInfo::with_role(Role::Server),
            Default::default(),
        ))
    }
}

newtype_impl_context!(ServerContext, Config, 0);

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

pub trait ThriftContext: volo::context::Context<Config = Config> + Send + 'static {
    fn encode_conn_reset(&self) -> Option<bool>;
    fn set_conn_reset_by_ttheader(&mut self, reset: bool);
    fn handle_decoded_msg_ident(&mut self, ident: &TMessageIdentifier);
    fn seq_id(&self) -> i32;
    fn msg_type(&self) -> TMessageType;
    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    fn stats(&self) -> &CommonStats;
    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    fn stats_mut(&mut self) -> &mut CommonStats;
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

    #[inline]
    fn stats(&self) -> &CommonStats {
        &self.common_stats
    }

    #[inline]
    fn stats_mut(&mut self) -> &mut CommonStats {
        &mut self.common_stats
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
        self.rpc_info_mut().set_method(ident.name.clone());
    }

    #[inline]
    fn seq_id(&self) -> i32 {
        self.seq_id.unwrap_or(0)
    }

    #[inline]
    fn msg_type(&self) -> TMessageType {
        self.msg_type.expect("`msg_type` should be set.")
    }

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
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_millis(50);
const DEFAULT_READ_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Default, Clone, Copy)]
pub struct Config {
    rpc_timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    read_write_timeout: Option<Duration>,
}

impl Config {
    #[inline]
    pub fn new() -> Self {
        Self {
            rpc_timeout: None,
            connect_timeout: None,
            read_write_timeout: None,
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

    #[inline]
    pub fn connect_timeout(&self) -> Option<Duration> {
        self.connect_timeout
    }

    #[inline]
    pub fn connect_timeout_or_default(&self) -> Duration {
        self.connect_timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT)
    }

    /// Sets the connect timeout.
    #[inline]
    pub fn set_connect_timeout(&mut self, timeout: Option<Duration>) {
        self.connect_timeout = timeout;
    }

    #[inline]
    pub fn read_write_timeout(&self) -> Option<Duration> {
        self.read_write_timeout
    }

    #[inline]
    pub fn read_write_timeout_or_default(&self) -> Duration {
        self.read_write_timeout
            .unwrap_or(DEFAULT_READ_WRITE_TIMEOUT)
    }

    #[inline]
    /// Sets the read write timeout(a.k.a. IO timeout).
    pub(crate) fn set_read_write_timeout(&mut self, timeout: Option<Duration>) {
        self.read_write_timeout = timeout;
    }

    #[inline]
    pub fn merge(&mut self, other: Self) {
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

impl Reusable for Config {
    fn clear(&mut self) {
        self.rpc_timeout = None;
        self.connect_timeout = None;
        self.read_write_timeout = None;
    }
}

impl ::volo::client::Apply<ClientContext> for CallOpt {
    type Error = crate::ClientError;

    #[inline]
    fn apply(self, cx: &mut ClientContext) -> Result<(), Self::Error> {
        let caller = cx.rpc_info.caller_mut();
        if !self.caller_faststr_tags.is_empty() {
            caller.faststr_tags.extend(self.caller_faststr_tags);
        }
        if !self.caller_tags.is_empty() {
            caller.tags.extend(self.caller_tags);
        }

        let callee = cx.rpc_info.callee_mut();
        if !self.callee_faststr_tags.is_empty() {
            callee.faststr_tags.extend(self.callee_faststr_tags);
        }
        if !self.callee_tags.is_empty() {
            callee.tags.extend(self.callee_tags);
        }
        if let Some(addr) = self.address {
            callee.set_address(addr);
        }
        cx.rpc_info.config_mut().merge(self.config);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Role, RpcInfo};
    use crate::context::ClientContext;

    #[test]
    fn test_rpcinfo() {
        let ri = &ClientContext::new(
            1,
            RpcInfo::with_role(Role::Client),
            pilota::thrift::TMessageType::Call,
        )
        .rpc_info;
        println!("{:?}", ri);
    }
}
