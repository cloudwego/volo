#![doc(
    html_logo_url = "https://github.com/cloudwego/volo/raw/main/.github/assets/logo.png?sanitize=true"
)]
#![cfg_attr(not(doctest), doc = include_str!("../README.md"))]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[macro_use]
mod cfg;

pub mod body;
pub mod client;
pub mod codec;
#[doc(hidden)]
pub mod codegen;
pub mod context;
pub mod layer;
pub mod message;
pub mod metadata;
pub mod request;
pub mod response;
pub mod server;
pub mod status;
pub mod transport;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type BoxStream<'l, T> = std::pin::Pin<Box<dyn futures::Stream<Item = T> + Send + Sync + 'l>>;

pub use client::Client;
pub use codec::decode::RecvStream;
pub use message::{RecvEntryMessage, SendEntryMessage};
pub use request::{IntoRequest, IntoStreamingRequest, Request};
pub use response::Response;
pub use status::{Code, Status};

pub(crate) const BASE64_ENGINE: base64::engine::GeneralPurpose =
    base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_encode_padding(false)
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
