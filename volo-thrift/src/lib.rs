#![feature(type_alias_impl_trait)]
#![feature(generic_associated_types)]
#![doc(
    html_logo_url = "https://github.com/cloudwego/volo/raw/main/.github/assets/logo.png?sanitize=true"
)]
#![cfg_attr(not(doctest), doc = include_str!("../README.md"))]

pub mod error;
mod message;
mod message_wrapper;
mod protocol;
pub mod transport;

pub mod client;
pub use client::Client;
pub mod codec;
pub mod context;
pub mod server;
pub mod tags;
pub use anyhow::Error as AnyhowError;
pub use error::*;
pub use message::{EntryMessage, Message};
pub use message_wrapper::*;
