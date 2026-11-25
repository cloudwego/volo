pub mod dial;
pub(crate) mod incoming;
#[cfg(feature = "multiplex")]
pub mod multiplex;
pub mod pingpong;
pub mod pool;
pub use dial::{DialPlan, SelectedTransport};
use pilota::thrift::ThriftException;
pub use pool::Config;

/// Set in the context extensions by the pingpong transports when a shmipc stream carried this
/// RPC's request or response over the session socket instead of shared memory (shmipc's
/// fallback mode). Absent when the payload went through shared memory or the call did not use
/// shmipc at all.
///
/// shmipc raises the flag on the data path only, so stream setup and control traffic never
/// count. On the server the flag is sticky for the connection: once a stream has fallen back,
/// every later RPC on it carries this marker, which matches the transport, since shmipc keeps
/// sending later responses over the socket.
#[cfg(feature = "shmipc")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShmipcPayloadFallback;

fn should_log(e: &ThriftException) -> bool {
    !matches!(e, ThriftException::Transport(te)
        if volo::util::remote_error::is_remote_closed_error(te.io_error())
            && !volo::util::remote_error::remote_closed_error_log_enabled()
    )
}
