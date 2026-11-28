use std::sync::Arc;

use bytes::Bytes;
use futures::future::BoxFuture;
use volo::{
    context::{Extensions, Role},
    streaming::{ContextDriver, Metadata, RpcError, RpcMode, RpcResult, StreamStatus},
};

use super::{Inner, StreamContext};
use crate::{Code, Status, metadata::MetadataMap};

fn method_payload(method: &crate::message::MethodDescriptor) -> volo::streaming::PayloadKind {
    match method.payload {
        crate::message::PayloadShape::ProtobufMessage => volo::streaming::PayloadKind::Protobuf,
        _ => volo::streaming::PayloadKind::Thrift,
    }
}

fn cancellation_error(status: Status) -> RpcError {
    use std::error::Error;

    if let Some(error) = status
        .source()
        .and_then(|source| source.downcast_ref::<RpcError>())
    {
        return error.clone();
    }
    status.into()
}

pub(crate) fn metadata_to_shared(metadata: &MetadataMap) -> Metadata {
    let mut result = Metadata::default();
    for (name, value) in metadata.headers() {
        result.append(
            Bytes::copy_from_slice(name.as_str().as_bytes()),
            Bytes::copy_from_slice(value.as_bytes()),
        );
    }
    result
}

pub(crate) fn metadata_from_shared(metadata: Metadata) -> Result<MetadataMap, Status> {
    if metadata.int_iter().next().is_some() {
        return Err(Status::invalid_argument(
            "TTHeader integer metadata cannot be represented as gRPC headers",
        ));
    }
    let mut headers = http::HeaderMap::new();
    for (name, value) in metadata {
        let name = http::HeaderName::from_bytes(&name)
            .map_err(|_| Status::invalid_argument("invalid gRPC metadata name"))?;
        let value = http::HeaderValue::from_bytes(&value)
            .map_err(|_| Status::invalid_argument("invalid gRPC metadata value"))?;
        headers.append(name, value);
    }
    Ok(MetadataMap::from_headers(headers))
}

impl ContextDriver for Inner {
    fn middleware(&self) -> Option<volo::streaming::ServerMiddleware> {
        self.middleware.clone()
    }
    fn hooks(&self) -> Option<&volo::streaming::CallHooks> {
        self.common_hooks.get()
    }
    fn install_hooks(&self, hooks: volo::streaming::CallHooks) -> bool {
        hooks.use_native_events();
        if self.common_hooks.set(hooks.clone()).is_err() {
            return false;
        }
        self.hook
            .get_or_init(super::Hook::default)
            .common
            .set(CommonHook(hooks))
            .is_ok()
    }
    fn install_scope(&self, scope: Arc<dyn volo::streaming::StreamScope>) -> bool {
        self.common_scope
            .set(volo::streaming::SharedScope(scope))
            .is_ok()
    }
    fn reclaimed(self: Arc<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            StreamContext(self).children_reclaimed().await;
        })
    }
    fn id(&self) -> u64 {
        self.id as u64
    }

    fn service(&self) -> &str {
        let path = self.method.strip_prefix('/').unwrap_or(&self.method);
        let service = path
            .split_once('/')
            .map(|(service, _)| service)
            .unwrap_or_default();
        service.rsplit('.').next().unwrap_or(service)
    }

    fn method(&self) -> &str {
        self.method.rsplit('/').next().unwrap_or(&self.method)
    }

    fn role(&self) -> Role {
        self.role
    }

    fn mode(&self) -> Option<RpcMode> {
        self.mode.map(|mode| match mode {
            crate::message::RpcMode::Unary => RpcMode::Unary,
            crate::message::RpcMode::ClientStreaming => RpcMode::ClientStreaming,
            crate::message::RpcMode::ServerStreaming => RpcMode::ServerStreaming,
            crate::message::RpcMode::Bidirectional => RpcMode::Bidirectional,
        })
    }

    fn payload(&self) -> Option<volo::streaming::PayloadKind> {
        let extensions = self.extensions.lock();
        extensions
            .get::<crate::message::MethodDescriptor>()
            .map(method_payload)
    }

    fn route(&self) -> Option<volo::streaming::RpcRoute<'_>> {
        if self.role != Role::Server {
            return None;
        }
        let method = *self
            .extensions
            .lock()
            .get::<crate::message::MethodDescriptor>()?;
        Some(volo::streaming::RpcRoute {
            service: self.service(),
            method: method.name,
            payload: method_payload(&method),
            mode: self.mode(),
        })
    }

    fn deadline(&self) -> Option<std::time::Instant> {
        self.deadline.map(|deadline| deadline.instant().into_std())
    }

    fn metadata(&self) -> Metadata {
        self.request_metadata
            .get()
            .map(metadata_to_shared)
            .unwrap_or_default()
    }

    fn response_headers(self: Arc<Self>, metadata: Metadata) -> RpcResult<()> {
        StreamContext(self)
            .response_headers(metadata_from_shared(metadata)?)
            .map_err(Into::into)
    }

    fn response_trailers(self: Arc<Self>, metadata: Metadata) -> RpcResult<()> {
        StreamContext(self)
            .response_trailers(metadata_from_shared(metadata)?)
            .map_err(Into::into)
    }

    fn is_finished(&self) -> bool {
        self.terminal.get().is_some()
    }
    fn status(&self) -> Option<StreamStatus> {
        self.terminal.get().map(|status| StreamStatus {
            error: (status.status.code() != Code::Ok).then(|| status.status.clone().into()),
            header: self
                .header
                .get()
                .map(metadata_to_shared)
                .unwrap_or_default(),
            trailer: self
                .trailer
                .get()
                .map(metadata_to_shared)
                .unwrap_or_default(),
            sent_messages: status.sent_messages as u64,
            sent_bytes: status.sent_bytes as u64,
            received_messages: status.received_messages as u64,
            received_bytes: status.received_bytes as u64,
        })
    }

    fn cancellation(&self) -> Option<RpcError> {
        self.producer_error().map(cancellation_error)
    }

    fn cancel(self: Arc<Self>, error: RpcError) {
        StreamContext(self).abort(error.into());
    }

    fn cancelled(self: Arc<Self>) -> BoxFuture<'static, RpcError> {
        Box::pin(async move { cancellation_error(StreamContext(self).producer_ended().await) })
    }

    fn is_entered(&self) -> bool {
        self.is_entered()
    }
    fn enter(self: Arc<Self>, operation: &mut dyn FnMut()) {
        StreamContext(self).sync_scope(operation);
    }

    fn with_extensions(&self, operation: &mut dyn FnMut(&mut Extensions)) {
        operation(&mut self.extensions.lock());
    }

    fn spawn_scoped(
        self: Arc<Self>,
        operation: BoxFuture<'static, ()>,
    ) -> BoxFuture<'static, RpcResult<()>> {
        let cx = StreamContext(self);
        let task = cx.spawn_scoped(operation);
        Box::pin(async move {
            match task.await {
                Ok(result) => result.map_err(Into::into),
                Err(error) if error.is_cancelled() => {
                    Err(cancellation_error(cx.producer_ended().await))
                }
                Err(_) => Err(Status::internal("managed RPC task panicked").into()),
            }
        })
    }
}

/// Public observers subscribe to the native Hook gate and terminal publisher.
pub(super) struct CommonHook(volo::streaming::CallHooks);
impl super::StreamHook for CommonHook {
    fn on_header(&self, cx: &StreamContext, metadata: &MetadataMap) {
        self.0.header(
            &cx.shared_context(),
            direction(cx),
            &metadata_to_shared(metadata),
        );
    }
    fn on_send(&self, cx: &StreamContext, size: usize) {
        self.0.send(&cx.shared_context(), size);
    }
    fn on_recv(&self, cx: &StreamContext, size: usize) {
        self.0.recv(&cx.shared_context(), size);
    }
    fn on_flow_control(&self, cx: &StreamContext, wait: std::time::Duration) {
        self.0.flow_control(&cx.shared_context(), wait);
    }
    fn on_trailer(&self, cx: &StreamContext, metadata: &MetadataMap) {
        let shared = cx.shared_context();
        self.0
            .trailer(&shared, direction(cx), &metadata_to_shared(metadata));
        self.0.half_close(&shared, direction(cx));
    }
    fn on_recv_closed(&self, cx: &StreamContext) {
        self.0
            .half_close(&cx.shared_context(), volo::streaming::StreamDirection::Recv);
    }
    fn on_io_error(&self, cx: &StreamContext, status: &Status) {
        self.0
            .io_error(&cx.shared_context(), &status.clone().into());
    }
    fn on_finish(&self, cx: &StreamContext, _: &super::StreamStatus) {
        self.0.close(&cx.shared_context());
    }
}
fn direction(cx: &StreamContext) -> volo::streaming::StreamDirection {
    if cx.role() == Role::Server {
        volo::streaming::StreamDirection::Send
    } else {
        volo::streaming::StreamDirection::Recv
    }
}
