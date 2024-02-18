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
}

impl CommonStats {
    #[inline]
    pub fn req_size(&self) -> Option<u64> {
        self.req_size
    }

    #[inline]
    pub fn set_req_size(&mut self, size: u64) {
        self.req_size = Some(size)
    }

    #[inline]
    pub fn resp_size(&self) -> Option<u64> {
        self.resp_size
    }

    #[inline]
    pub fn set_resp_size(&mut self, size: u64) {
        self.resp_size = Some(size)
    }

    #[inline]
    pub fn reset(&mut self) {
        *self = Self { ..Self::default() }
    }
}
