//! Used to make underlying connection to other endpoints.

pub(crate) mod bdp;
mod client;
mod connect;
pub(crate) mod h2_body;
mod h2_client;

pub use client::ClientTransport;
