//! HTTP transport related utilities

mod connector;
#[cfg(feature = "http1")]
pub(crate) mod http1;
#[cfg(feature = "http2")]
pub(crate) mod http2;
mod plain;
pub(crate) mod pool;
pub mod protocol;
#[cfg(feature = "__tls")]
mod tls;
