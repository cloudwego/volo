use std::error::Error;

pub(crate) mod client;

pub use self::client::ClientError;

pub type BoxError = Box<dyn Error + Send + Sync>;
