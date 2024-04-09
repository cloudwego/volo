#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "client")]
pub use self::client::ClientContext;

#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "server")]
pub use self::server::{RequestPartsExt, ServerContext};
