//! Json utilities of Volo-HTTP

use serde::{de::DeserializeOwned, ser::Serialize};
pub use sonic_rs::Error;

pub fn serialize<T>(data: &T) -> Result<Vec<u8>, Error>
where
    T: Serialize,
{
    sonic_rs::to_vec(data)
}

#[cfg(feature = "server")]
pub fn serialize_to_writer<W, T>(writer: W, data: &T) -> Result<(), Error>
where
    W: sonic_rs::writer::WriteExt,
    T: Serialize,
{
    sonic_rs::to_writer(writer, data)
}

pub fn deserialize<T>(data: &[u8]) -> Result<T, Error>
where
    T: DeserializeOwned,
{
    sonic_rs::from_slice(data)
}
