//! Collections of some useful [`Layer`]s.
//!
//! [`Layer`]: motore::layer::Layer

mod fail_on_status;
pub mod header;
mod timeout;

pub use self::{
    fail_on_status::{FailOnStatus, StatusCodeError},
    timeout::Timeout,
};
