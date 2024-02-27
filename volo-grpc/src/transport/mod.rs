//! Used to make underlying connection to other endpoints.

mod client;
mod connect;
#[cfg(feature = "__tls")]
mod tls;

pub use client::ClientTransport;
#[cfg(feature = "__tls")]
pub use tls::{ServerTlsConfig, TlsAcceptor};
