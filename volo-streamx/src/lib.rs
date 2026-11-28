//! Shared business interfaces and generated protocol bindings for StreamX.

mod registry;
mod server;
pub use server::{
    ConnectionEvent, ConnectionObserver, ConnectionStats, ConnectionTransport, GrpcServer,
    PreparedServer, Protocol, Protocols, Rejection, Server, ServerHandle, ThriftServer,
};
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
#[doc(hidden)]
pub use registry::ServiceRegistration;
pub use registry::{Binding, FrozenRegistry, MethodBinding, RegistrationError, Registry};
pub use volo::streaming::{
    CancelCause, GrpcStatus, Invocation, InvocationHandler, InvocationService, IoPhase, Metadata,
    PayloadKind, RequestStream, ResponseSender, RpcError, RpcMode, RpcResult, RpcRoute,
    StreamContext, StreamDirection, StreamHook, StreamHookFactory, StreamScope, StreamStatus,
};

/// One method declaration, shared by every protocol binding of a service.
#[derive(Clone, Copy, Debug)]
pub struct MethodDescriptor {
    pub name: &'static str,
    pub mode: RpcMode,
    pub oneway: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ServiceDescriptor {
    pub name: &'static str,
    pub wire_name: &'static str,
    pub payload: PayloadKind,
    pub methods: &'static [MethodDescriptor],
}

/// Dependencies used by generated bindings, independent of the build plugin.
#[doc(hidden)]
pub mod codegen {
    pub use pilota;
    pub use volo;
    pub use volo_grpc as grpc;
    pub use volo_thrift as thrift;
}
