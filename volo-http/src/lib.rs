#![doc(
    html_logo_url = "https://github.com/cloudwego/volo/raw/main/.github/assets/logo.png?sanitize=true"
)]
#![cfg_attr(not(doctest), doc = include_str!("../README.md"))]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![deny(missing_docs)]

pub mod body;
#[cfg(feature = "client")]
pub mod client;
pub mod context;
pub mod error;
pub mod request;
pub mod response;
#[cfg(feature = "server")]
pub mod server;
pub mod utils;

#[doc(hidden)]
pub mod prelude {
    pub use bytes::Bytes;
    pub use http;
    pub use hyper;
    pub use volo::net::Address;

    #[cfg(feature = "client")]
    pub use crate::client::prelude::*;
    #[cfg(feature = "server")]
    pub use crate::server::prelude::*;
}

#[doc(hidden)]
pub use self::prelude::*;

#[cfg(not(any(feature = "http1", feature = "http2")))]
compile_error!("At least one of features \"http1\" and \"http2\" needs to be enabled!");
