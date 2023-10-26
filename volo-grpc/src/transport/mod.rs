//! Used to make underlying connection to other endpoints.

mod client;
mod connect;

cfg_rustls_or_native_tls! {
    mod tls;
    pub use tls::{ServerTlsConfig, TlsAcceptor};
}

pub use client::ClientTransport;
