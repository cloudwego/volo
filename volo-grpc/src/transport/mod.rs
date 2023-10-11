//! Used to make underlying connection to other endpoints.

mod client;
mod connect;
mod tls;

pub use client::ClientTransport;

pub(crate) use tls::TlsAcceptorConfig;