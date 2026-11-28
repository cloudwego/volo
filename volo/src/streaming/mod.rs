//! Shared streaming contracts and transport-independent service lookup.

mod context;
mod directory;
mod drain;
mod error;
mod hooks;
mod middleware;
mod stream;

#[doc(hidden)]
pub use context::ContextDriver;
pub use context::{Metadata, PayloadKind, RpcMode, RpcRoute, StreamContext, StreamStatus};
pub use directory::{RegistrationError, ServiceDirectory, ServiceEntry};
#[doc(hidden)]
pub use drain::{NativeConnection, ServerDrain};
pub use error::{CancelCause, GrpcStatus, IoPhase, RpcError, RpcResult};
#[doc(hidden)]
pub use hooks::CallHooks;
pub use hooks::{StreamDirection, StreamHook, StreamHookFactory};
pub use middleware::{Invocation, InvocationHandler, InvocationService, StreamScope};
#[doc(hidden)]
pub use middleware::{ServerMiddleware, SharedScope};
#[doc(hidden)]
pub use stream::ResponseSink;
pub use stream::{RequestStream, ResponseSender};
