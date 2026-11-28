//! TTHeader streaming codecs, native lifecycle state and transport integration.
//! Generated server handlers use the shared business types in `volo::streaming`.
//! Runtime transport is enabled by `ttstream`.
mod codec;
mod context;
mod error;
mod hooks;
mod metadata;
mod options;
pub(crate) mod resources;
mod scope;
mod shared;
pub(crate) mod unary;
pub use codec::{decode_exception, decode_message, encode_exception, encode_message};
pub(crate) use context::{DirtyStreams, DriverNotify, Inner as StreamInner, STREAM_CONTEXT};
pub use context::{StreamContext, StreamResourceMonitor};
pub use error::{CancelCause, IoDirection, IoPhase, StreamError, StreamIoError, StreamStatus};
pub use hooks::{HookChain, NoopHooks, StreamHook};
pub(crate) use metadata::copy_outgoing_metainfo;
pub use metadata::{Metadata, export_metainfo, import_metainfo};
pub use options::{PoolMode, RecvTimeoutPolicy, StreamConfig, StreamMode, StreamingProtocol};
pub use resources::StreamResources;
pub use scope::{StreamHandlerFuture, StreamHandlerScope};
#[doc(hidden)]
pub use unary::dispatch_unary;

pub(crate) mod stream;
pub use stream::{
    BidiStreaming, ClientStreaming, OpenedStream, RequestSender, RequestStream, ResponseSender,
    ResponseStream, SendReservation, ServerStreaming,
};

pub(crate) mod open;
pub use open::{OpenService, OpenStream};
pub use stream::{RawSender, RawStream};
