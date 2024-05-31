use serde::{de::DeserializeOwned, ser::Serialize};
#[cfg(feature = "serde_json")]
pub use serde_json::Error;
#[cfg(feature = "sonic_json")]
pub use sonic_rs::Error;

#[cfg(all(feature = "serde_json", feature = "sonic_json"))]
compile_error!("features `serde_json` and `sonic_json` cannot be enabled at the same time.");

#[cfg(feature = "server")]
mod server;

pub(crate) fn serialize<T>(data: &T) -> Result<Vec<u8>, Error>
where
    T: Serialize,
{
    #[cfg(feature = "sonic_json")]
    let res = sonic_rs::to_vec(data);

    #[cfg(feature = "serde_json")]
    let res = serde_json::to_vec(data);

    res
}

pub(crate) fn deserialize<T>(data: &[u8]) -> Result<T, Error>
where
    T: DeserializeOwned,
{
    #[cfg(feature = "sonic_json")]
    let res = sonic_rs::from_slice(data);

    #[cfg(feature = "serde_json")]
    let res = serde_json::from_slice(data);

    res
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Json<T>(pub T);
