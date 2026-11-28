use std::sync::Arc;

use futures::future::BoxFuture;
use pilota::thrift::{ApplicationException, ApplicationExceptionKind, Message};
use volo::{
    context::{Extensions, Role},
    streaming::{ContextDriver, Metadata, RpcError, RpcMode, RpcResult, StreamStatus},
};

use super::{CancelCause, ResponseSender, StreamContext, StreamError, context::Inner};

pub(super) fn metadata_to_shared(metadata: &super::Metadata) -> Metadata {
    let mut result = Metadata::default();
    for (&key, value) in &metadata.int {
        result.insert_int(key, value.clone());
    }
    for (key, value) in &metadata.string {
        result.append(key.clone(), value.clone());
    }
    result
}

fn metadata_from_shared(metadata: Metadata) -> super::Metadata {
    let mut result = super::Metadata::default();
    result
        .int
        .extend(metadata.int_iter().map(|(key, value)| (key, value.clone())));
    for (key, value) in metadata {
        result.insert(key, value);
    }
    result
}

impl From<StreamError> for RpcError {
    fn from(error: StreamError) -> Self {
        match error {
            StreamError::Biz {
                code,
                message,
                extra,
            } => Self::Biz {
                code,
                message,
                extra,
            },
            StreamError::Application(error) => Self::Application {
                kind: error.kind().as_i32(),
                message: error.message().to_string(),
            },
            StreamError::Canceled {
                cause,
                code,
                message,
                path,
            } => Self::Canceled {
                cause,
                code,
                message,
                path,
            },
            StreamError::Transport {
                source,
                phase,
                committed,
            } => Self::Transport {
                source,
                phase,
                committed,
            },
            StreamError::Protocol(message) => Self::Protocol(message),
            StreamError::ProtocolStatus { code, message } => Self::ProtocolStatus { code, message },
            StreamError::ResourceExhausted {
                scope,
                limit,
                observed,
            } => Self::ResourceExhausted {
                scope,
                limit,
                observed,
            },
            StreamError::Configuration(message) => Self::Configuration(message),
            StreamError::ServerDraining => Self::ServerDraining,
            StreamError::RecvTimeout => Self::RecvTimeout,
            StreamError::Rpc(error) => *error,
        }
    }
}

impl From<RpcError> for StreamError {
    fn from(error: RpcError) -> Self {
        match error {
            RpcError::Biz {
                code,
                message,
                extra,
            } => Self::Biz {
                code,
                message,
                extra,
            },
            RpcError::Application { kind, message } => Self::Application(
                ApplicationException::new(ApplicationExceptionKind::from_i32(kind), message),
            ),
            RpcError::Canceled {
                cause,
                code,
                message,
                path,
            } => {
                let code = if code == 0 {
                    match cause {
                        CancelCause::Deadline => 12015,
                        CancelCause::ConnectionClosed => 12013,
                        CancelCause::Drop => 12011,
                        CancelCause::ServerShutdown => 6,
                        _ => 12007,
                    }
                } else {
                    code
                };
                Self::Canceled {
                    cause,
                    code,
                    message,
                    path,
                }
            }
            RpcError::Transport {
                source,
                phase,
                committed,
            } => Self::Transport {
                source,
                phase,
                committed,
            },
            RpcError::Protocol(message) => Self::Protocol(message),
            RpcError::ProtocolStatus { code, message } => Self::ProtocolStatus { code, message },
            RpcError::ResourceExhausted {
                scope,
                limit,
                observed,
            } => Self::ResourceExhausted {
                scope,
                limit,
                observed,
            },
            RpcError::Configuration(message) => Self::Configuration(message),
            RpcError::ServerDraining => Self::ServerDraining,
            RpcError::RecvTimeout => Self::RecvTimeout,
            error => Self::Rpc(Box::new(error)),
        }
    }
}

impl<T: Message + 'static> volo::streaming::ResponseSink<T> for ResponseSender<T> {
    fn prepare(&mut self, value: &T) -> RpcResult<()> {
        self.prepare_shared(value).map_err(Into::into)
    }
    fn poll_send(&mut self, task: &mut std::task::Context<'_>) -> std::task::Poll<RpcResult<()>> {
        self.poll_shared(task).map_err(Into::into)
    }
    fn abort_send(&mut self) {
        self.abort_shared();
    }
}

impl ContextDriver for Inner {
    fn middleware(&self) -> Option<volo::streaming::ServerMiddleware> {
        self.middleware.clone()
    }
    fn hooks(&self) -> Option<&volo::streaming::CallHooks> {
        self.common_hooks.get()
    }
    fn install_hooks(&self, hooks: volo::streaming::CallHooks) -> bool {
        if self.mode.load(std::sync::atomic::Ordering::Acquire) != 4 {
            hooks.use_native_events();
        }
        self.common_hooks.set(hooks).is_ok()
    }
    fn install_scope(&self, scope: Arc<dyn volo::streaming::StreamScope>) -> bool {
        self.common_scope
            .set(volo::streaming::SharedScope(scope))
            .is_ok()
    }
    fn reclaimed(self: Arc<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            StreamContext { inner: self }.tasks_reclaimed().await;
        })
    }
    fn id(&self) -> u64 {
        self.id as u32 as u64
    }
    fn service(&self) -> &str {
        &self.service
    }
    fn method(&self) -> &str {
        &self.method
    }
    fn role(&self) -> Role {
        self.rpc_info.lock().role()
    }
    fn mode(&self) -> Option<RpcMode> {
        match self.mode.load(std::sync::atomic::Ordering::Acquire) {
            1 => Some(RpcMode::ClientStreaming),
            2 => Some(RpcMode::ServerStreaming),
            3 => Some(RpcMode::Bidirectional),
            4 => Some(RpcMode::Unary),
            _ => None,
        }
    }
    fn payload(&self) -> Option<volo::streaming::PayloadKind> {
        Some(volo::streaming::PayloadKind::Thrift)
    }
    fn route(&self) -> Option<volo::streaming::RpcRoute<'_>> {
        (self
            .registered_method
            .load(std::sync::atomic::Ordering::Acquire)
            && self.role() == Role::Server)
            .then(|| volo::streaming::RpcRoute {
                service: &self.service,
                method: &self.method,
                payload: volo::streaming::PayloadKind::Thrift,
                mode: self.mode(),
            })
    }
    fn deadline(&self) -> Option<std::time::Instant> {
        *self.deadline.lock()
    }
    fn metadata(&self) -> Metadata {
        metadata_to_shared(&self.metadata.lock())
    }
    fn response_headers(self: Arc<Self>, metadata: Metadata) -> RpcResult<()> {
        if self.rpc_info.lock().role() != Role::Server {
            return Err(RpcError::Configuration(
                "response metadata requires a server call".into(),
            ));
        }
        StreamContext { inner: self }
            .set_outgoing_metadata(metadata_from_shared(metadata), false)
            .map_err(Into::into)
    }
    fn response_trailers(self: Arc<Self>, metadata: Metadata) -> RpcResult<()> {
        if self.rpc_info.lock().role() != Role::Server {
            return Err(RpcError::Configuration(
                "response metadata requires a server call".into(),
            ));
        }
        StreamContext { inner: self }
            .set_outgoing_metadata(metadata_from_shared(metadata), true)
            .map_err(Into::into)
    }
    fn is_finished(&self) -> bool {
        self.status.lock().is_some()
    }
    fn status(&self) -> Option<StreamStatus> {
        let status = self.status.lock().clone()?;
        let server = self.rpc_info.lock().role() == Role::Server;
        let (header, trailer) = if server {
            let outgoing = self.outgoing.lock();
            (
                metadata_to_shared(&outgoing.header),
                metadata_to_shared(&outgoing.trailer),
            )
        } else {
            (
                metadata_to_shared(&status.header),
                metadata_to_shared(&status.trailer),
            )
        };
        Some(StreamStatus {
            error: status.error.map(Into::into),
            header,
            trailer,
            sent_messages: status.sent_messages,
            sent_bytes: status.sent_bytes,
            received_messages: status.received_messages,
            received_bytes: status.received_bytes,
        })
    }
    fn cancellation(&self) -> Option<RpcError> {
        self.producer_error().map(Into::into)
    }
    fn cancel(self: Arc<Self>, error: RpcError) {
        StreamContext { inner: self }.cancel(error.into());
    }
    fn cancelled(self: Arc<Self>) -> BoxFuture<'static, RpcError> {
        Box::pin(async move { StreamContext { inner: self }.ended().await.into() })
    }
    fn is_entered(&self) -> bool {
        self.is_entered()
    }
    fn enter(self: Arc<Self>, operation: &mut dyn FnMut()) {
        StreamContext { inner: self }.sync_scope(operation);
    }
    fn with_extensions(&self, operation: &mut dyn FnMut(&mut Extensions)) {
        operation(&mut self.extensions.lock());
    }
    fn spawn_scoped(
        self: Arc<Self>,
        operation: BoxFuture<'static, ()>,
    ) -> BoxFuture<'static, RpcResult<()>> {
        let cx = StreamContext { inner: self };
        let task = cx.spawn_scoped(operation);
        Box::pin(async move {
            match task.await {
                Ok(result) => result.map_err(Into::into),
                Err(error) if error.is_cancelled() => Err(cx.ended().await.into()),
                Err(_) => Err(RpcError::Application {
                    kind: ApplicationExceptionKind::INTERNAL_ERROR.as_i32(),
                    message: "managed stream task panicked".into(),
                }),
            }
        })
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;

    #[test]
    fn integer_metadata_survives_the_shared_context_round_trip() {
        let mut native = super::super::Metadata::default();
        native
            .int
            .insert(42, bytes::Bytes::from_static(b"binary\0value"));
        native.insert(bytes::Bytes::from_static(b"name"), "value");
        let shared = metadata_to_shared(&native);
        assert_eq!(shared.get_int(42), native.int.get(&42));
        let returned = metadata_from_shared(shared);
        assert_eq!(returned.int, native.int);
        assert_eq!(returned.string, native.string);
    }
}
