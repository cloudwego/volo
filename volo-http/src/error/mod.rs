//! Generic error types

use std::error::Error;

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "client")]
pub use self::client::ClientError;

#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "server")]
pub use self::server::ExtractBodyError;

/// Boxed [`Error`] with [`Send`] and [`Sync`]
pub type BoxError = Box<dyn Error + Send + Sync>;
