//! Cookie utilities of Volo-HTTP.
//!
//! [`CookieJar`] currently only supports the server side.

mod jar;
mod store;

pub use cookie::{time::Duration, Cookie};
pub use jar::CookieJar;
#[cfg(feature = "client")]
pub(crate) use store::CookieStore;
