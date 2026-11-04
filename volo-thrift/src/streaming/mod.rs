//! Protocol-neutral streaming API. Runtime transport is enabled by `ttstream`.
mod codec;
mod context;
mod error;
mod hooks;
mod metadata;
mod options;
mod raw;
pub(crate) mod resources;
mod scope;
pub use codec::{decode_exception, decode_message, encode_exception, encode_message};
pub use context::{STREAM_CONTEXT, StreamContext, StreamResourceMonitor};
pub use error::{CancelCause, IoDirection, IoPhase, StreamError, StreamIoError, StreamStatus};
pub use hooks::{HookChain, NoopHooks, StreamHook};
pub(crate) use metadata::copy_outgoing_metainfo;
pub use metadata::{Metadata, export_metainfo, import_metainfo};
pub use options::{PoolMode, RecvTimeoutPolicy, StreamConfig, StreamMode, StreamingProtocol};
pub use raw::RawStruct;
pub use resources::StreamResources;
pub use scope::{StreamHandlerFuture, StreamHandlerScope};

pub(crate) mod stream;
pub use stream::{
    BidiStreaming, ClientStreaming, OpenedStream, RequestSender, RequestStream, ResponseSender,
    ResponseStream, SendReservation, ServerStreaming,
};

pub(crate) mod open;
pub use open::{OpenService, OpenStream};
pub use stream::{ConversionReservation, PreparedMessage, RawSender, RawStream, ReceiveValidation};
