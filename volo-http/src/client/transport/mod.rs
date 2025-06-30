//! HTTP transport related utilities

mod connector;
#[cfg(feature = "http1")]
pub mod http1;
#[cfg(feature = "http2")]
pub mod http2;
mod plain;
pub(crate) mod pool;
pub mod protocol;
#[cfg(feature = "__tls")]
mod tls;
