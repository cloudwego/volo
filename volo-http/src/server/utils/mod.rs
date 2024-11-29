//! Utilities at server-side.

mod file_response;
mod serve_dir;

pub use file_response::FileResponse;
pub use serve_dir::ServeDir;

pub mod client_ip;
#[cfg(feature = "multipart")]
pub mod multipart;
#[cfg(feature = "ws")]
pub mod ws;
