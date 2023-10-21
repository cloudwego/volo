#![doc(
    html_logo_url = "https://github.com/cloudwego/volo/raw/main/.github/assets/logo.png?sanitize=true"
)]
#![cfg_attr(not(doctest), doc = include_str!("../README.md"))]

pub use motore::{layer, layer::Layer, service, Service};
pub use tokio::main;

pub mod context;
pub mod discovery;
pub mod loadbalance;
pub mod net;
pub mod util;
pub use hack::Unwrap;
#[cfg(target_family = "unix")]
pub mod hotrestart;

pub mod client;
mod hack;
mod macros;

pub use faststr::FastStr;
pub use metainfo::METAINFO;

/// volo::spawn will spawn a task and derive the metainfo
pub fn spawn<T>(future: T) -> tokio::task::JoinHandle<T::Output>
where
    T: futures::Future + Send + 'static,
    T::Output: Send + 'static,
{
    let mi = METAINFO
        .try_with(|m| {
            let prev_mi = m.take();
            let (m1, m2) = prev_mi.derive();
            m.replace(m1);
            m2
        })
        .unwrap_or_else(|_| metainfo::MetaInfo::new());

    tokio::spawn(METAINFO.scope(std::cell::RefCell::new(mi), future))
}
