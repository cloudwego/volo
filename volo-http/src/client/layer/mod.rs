//! Collections of some useful [`Layer`]s.
//!
//! [`Layer`]: motore::layer::Layer

mod fail_on_status;
pub mod header;
#[cfg(feature = "http1")]
pub mod http_proxy;
mod redirect;
mod timeout;
pub(crate) mod utils;

pub use self::{
    fail_on_status::{FailOnStatus, StatusCodeError},
    redirect::{FollowRedirect, RedirectPredicate},
    timeout::Timeout,
    utils::TargetLayer,
};
