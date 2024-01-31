macro_rules! impl_deref_and_deref_mut {
    ($type:ty, $inner:ty, $pos:tt) => {
        impl std::ops::Deref for $type {
            type Target = $inner;

            #[inline]
            fn deref(&self) -> &Self::Target {
                &self.$pos
            }
        }

        impl std::ops::DerefMut for $type {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.$pos
            }
        }
    };
}

macro_rules! impl_getter {
    ($name: ident, $type: ty, $($path: tt).+) => {
        paste! {
            #[inline]
            pub fn $name(&self) -> &$type {
                &self.$($path).+
            }

            #[inline]
            pub fn [<$name _mut>](&mut self) -> &mut $type {
                &mut self.$($path).+
            }
        }
    };
    ($name: ident, $type: ty) => {
        paste! {
            #[inline]
            pub fn $name(&self) -> &$type {
                &self.$name
            }

            #[inline]
            pub fn [<$name _mut>](&mut self) -> &mut $type {
                &mut self.$name
            }
        }
    };
}

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

pub mod client;
pub mod server;

pub use self::{
    client::ClientContext,
    server::{ConnectionInfo, ServerContext},
};

pub type HttpContext = ServerContext;

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
