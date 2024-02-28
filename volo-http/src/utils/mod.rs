#![allow(unused)]
pub mod consts;
pub mod macros;
mod service_fn;

pub use self::service_fn::{service_fn, Callback};
