use std::error::Error;

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "client")]
pub use self::client::ClientError;

#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "server")]
pub use self::server::RejectionError;

pub type BoxError = Box<dyn Error + Send + Sync>;
