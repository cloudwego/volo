#![feature(generic_associated_types)]
#![feature(type_alias_impl_trait)]
#![feature(once_cell)]
#![doc(
    html_logo_url = "https://github.com/cloudwego/volo/raw/main/.github/assets/logo.png?sanitize=true"
)]
#![cfg_attr(not(doctest), doc = include_str!("../README.md"))]

pub use async_trait::async_trait;
pub use motore::{layer, layer::Layer, service, Service};
pub use tokio::{main, spawn};

pub mod context;
pub mod discovery;
pub mod loadbalance;
pub mod net;
pub mod util;
pub use hack::Unwrap;

mod hack;
mod macros;
