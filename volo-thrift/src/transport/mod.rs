pub mod dial;
pub(crate) mod incoming;
#[cfg(feature = "multiplex")]
pub mod multiplex;
pub mod pingpong;
pub mod pool;
pub use dial::{DialPlan, SelectedTransport};
use pilota::thrift::ThriftException;
pub use pool::Config;

use crate::context::ClientContext;

/// How this RPC fell back from its preferred transport. It is written into the context extensions
/// by the pingpong transports, and absent when neither side of the call used a fallback path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransportFallback {
    /// The [`DialPlan`] primary (for example shmipc) could not be acquired and a later candidate
    /// (UDS/TCP) served the call. Client side only.
    pub connection: bool,
    /// A shmipc stream carried the request or the response payload over its socket instead of
    /// shared memory. On the server the flag is sticky for the connection: once a stream
    /// has fallen back, every later RPC on it reports `true` (shmipc keeps sending later responses
    /// over the socket).
    pub payload: bool,
}

impl TransportFallback {
    /// Whether any fallback path was used.
    pub fn any(&self) -> bool {
        self.connection || self.payload
    }

    /// Whether the transport acquired for `cx` came from a later [`DialPlan`] candidate.
    pub(crate) fn connection_fallback(cx: &ClientContext) -> bool {
        cx.extensions
            .get::<SelectedTransport>()
            .is_some_and(|selected| selected.attempt() > 0)
    }

    /// Records the fallback state of the current RPC in `cx`: the dial-plan outcome plus the
    /// given shmipc payload flag. Nothing is inserted when neither applies, so absence means no
    /// fallback; a later call overwrites an earlier one.
    pub(crate) fn record(cx: &mut ClientContext, payload: bool) {
        let fallback = Self {
            connection: Self::connection_fallback(cx),
            payload,
        };
        if fallback.any() {
            cx.extensions.insert(fallback);
        }
    }
}

fn should_log(e: &ThriftException) -> bool {
    !matches!(e, ThriftException::Transport(te)
        if volo::util::remote_error::is_remote_closed_error(te.io_error())
            && !volo::util::remote_error::remote_closed_error_log_enabled()
    )
}

#[cfg(test)]
mod tests {
    use pilota::thrift::TMessageType;
    use volo::context::{Role, RpcInfo};

    use super::*;

    fn cx() -> ClientContext {
        ClientContext::new(1, RpcInfo::with_role(Role::Client), TMessageType::Call)
    }

    fn addr() -> volo::net::Address {
        volo::net::Address::from("127.0.0.1:1".parse::<std::net::SocketAddr>().unwrap())
    }

    #[test]
    fn connection_fallback_follows_selected_attempt() {
        let mut cx = cx();
        assert!(!TransportFallback::connection_fallback(&cx));
        cx.extensions.insert(SelectedTransport::new(addr(), 0));
        assert!(!TransportFallback::connection_fallback(&cx));
        cx.extensions.insert(SelectedTransport::new(addr(), 1));
        assert!(TransportFallback::connection_fallback(&cx));
    }

    #[test]
    fn record_inserts_only_when_a_fallback_happened() {
        let mut cx = cx();
        TransportFallback::record(&mut cx, false);
        assert!(cx.extensions.get::<TransportFallback>().is_none());

        TransportFallback::record(&mut cx, true);
        assert_eq!(
            cx.extensions.get::<TransportFallback>(),
            Some(&TransportFallback {
                connection: false,
                payload: true
            })
        );

        cx.extensions.insert(SelectedTransport::new(addr(), 1));
        TransportFallback::record(&mut cx, false);
        assert_eq!(
            cx.extensions.get::<TransportFallback>(),
            Some(&TransportFallback {
                connection: true,
                payload: false
            })
        );
    }

    #[test]
    fn any_is_either_source() {
        assert!(!TransportFallback::default().any());
        assert!(
            TransportFallback {
                connection: true,
                payload: false
            }
            .any()
        );
        assert!(
            TransportFallback {
                connection: false,
                payload: true
            }
            .any()
        );
    }
}
