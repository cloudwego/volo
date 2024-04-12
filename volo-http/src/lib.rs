#![doc(
    html_logo_url = "https://github.com/cloudwego/volo/raw/main/.github/assets/logo.png?sanitize=true"
)]
#![cfg_attr(not(doctest), doc = include_str!("../README.md"))]

pub mod body;
#[cfg(feature = "client")]
pub mod client;
pub mod context;
#[cfg(feature = "cookie")]
pub mod cookie;
pub mod error;
pub mod extension;
#[cfg(feature = "__json")]
pub mod json;
pub mod request;
pub mod response;
#[cfg(feature = "server")]
pub mod server;

pub(crate) mod utils;

#[doc(hidden)]
pub mod prelude {
    pub use bytes::Bytes;
    pub use http;
    pub use hyper;
    pub use volo::net::Address;

    #[cfg(feature = "client")]
    pub use crate::client::prelude::*;
    pub use crate::extension::Extension;
    #[cfg(feature = "__json")]
    pub use crate::json::Json;
    #[cfg(feature = "server")]
    pub use crate::server::prelude::*;
}

pub use self::prelude::*;
