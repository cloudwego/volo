use std::error::Error;

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "client")]
pub use self::client::ClientError;

pub type BoxError = Box<dyn Error + Send + Sync>;
