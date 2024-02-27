use http::StatusCode;
use paste::paste;

// This macro is unused only when both `client` and `server` features are not enabled.
// But no one can use this crate without any of them, maybe :)
#[allow(unused_macros)]
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

macro_rules! stat_impl_getter_and_setter {
    ($name: ident, $type: ty) => {
        paste! {
            #[inline]
            pub fn $name(&self) -> Option<&$type> {
                self.$name.as_ref()
            }

            #[inline]
            pub fn [<set_ $name>](&mut self, t: $type) {
                self.$name = Some(t)
            }
        }
    };
}

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "client")]
pub use self::client::ClientContext;

#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "server")]
pub use self::server::{ConnectionInfo, ServerContext};

/// This is unstable now and may be changed in the future.
#[derive(Debug, Default, Clone, Copy)]
pub struct CommonStats {
    req_size: Option<u64>,
    resp_size: Option<u64>,
    status_code: Option<StatusCode>,
}

impl CommonStats {
    stat_impl_getter_and_setter!(req_size, u64);
    stat_impl_getter_and_setter!(resp_size, u64);
    stat_impl_getter_and_setter!(status_code, StatusCode);

    #[inline]
    pub fn reset(&mut self) {
        *self = Self { ..Self::default() }
    }
}
