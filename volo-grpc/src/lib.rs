#![doc(
    html_logo_url = "https://github.com/cloudwego/volo/raw/main/.github/assets/logo.png?sanitize=true"
)]
#![cfg_attr(not(doctest), doc = include_str!("../README.md"))]
#![feature(generic_associated_types)]
#![feature(type_alias_impl_trait)]

pub mod body;
pub mod client;
pub mod codec;
#[doc(hidden)]
pub mod codegen;
pub mod context;
pub mod layer;
mod message;
pub mod metadata;
pub mod request;
pub mod response;
pub mod server;
pub mod status;
pub mod transport;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type BoxStream<'l, T> = std::pin::Pin<Box<dyn futures_core::Stream<Item = T> + Send + 'l>>;

pub use codec::decode::RecvStream;
pub use message::{RecvEntryMessage, SendEntryMessage};
pub use request::{IntoRequest, IntoStreamingRequest, Request};
pub use response::Response;
pub use status::{Code, Status};
