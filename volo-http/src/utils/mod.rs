//! Utilities of Volo-HTTP.

pub mod consts;
#[cfg(feature = "cookie")]
pub mod cookie;
mod extension;
#[cfg(feature = "json")]
pub(crate) mod json;
#[cfg(feature = "client")]
pub(crate) mod lazy;
pub(crate) mod macros;
#[cfg(test)]
pub mod test_helpers;

pub use self::extension::Extension;
