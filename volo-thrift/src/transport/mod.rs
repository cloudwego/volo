pub mod dial;
pub(crate) mod incoming;
#[cfg(feature = "multiplex")]
pub mod multiplex;
pub mod pingpong;
pub mod pool;
pub use dial::{DialPlan, SelectedTransport};
use pilota::thrift::ThriftException;
pub use pool::Config;

fn should_log(e: &ThriftException) -> bool {
    !matches!(e, ThriftException::Transport(te)
        if volo::util::remote_error::is_remote_closed_error(te.io_error())
            && !volo::util::remote_error::remote_closed_error_log_enabled()
    )
}
