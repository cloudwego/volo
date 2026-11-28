use std::{future::Future, sync::Arc, time::Instant};

use bytes::Bytes;
use futures::future::BoxFuture;

use super::{RpcError, RpcResult};
use crate::context::{Extensions, Role};

/// Ordered metadata with byte values and repeated keys preserved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata(Vec<(Bytes, Bytes)>, std::collections::BTreeMap<u16, Bytes>);

impl Metadata {
    pub fn get(&self, key: &str) -> Option<&Bytes> {
        self.0
            .iter()
            .find(|(name, _)| name.as_ref() == key.as_bytes())
            .map(|(_, value)| value)
    }

    pub fn insert(&mut self, key: impl Into<Bytes>, value: impl Into<Bytes>) {
        let key = key.into();
        self.0.retain(|(name, _)| name != &key);
        self.0.push((key, value.into()));
    }

    pub fn append(&mut self, key: impl Into<Bytes>, value: impl Into<Bytes>) {
        self.0.push((key.into(), value.into()));
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Bytes, &Bytes)> {
        self.0.iter().map(|(key, value)| (key, value))
    }

    pub fn get_all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Bytes> {
        self.0
            .iter()
            .filter(move |(name, _)| name.as_ref() == key.as_bytes())
            .map(|(_, value)| value)
    }

    /// Remove every value associated with this key.
    pub fn remove(&mut self, key: &str) {
        self.0.retain(|(name, _)| name.as_ref() != key.as_bytes());
    }

    pub fn get_int(&self, key: u16) -> Option<&Bytes> {
        self.1.get(&key)
    }
    pub fn insert_int(&mut self, key: u16, value: impl Into<Bytes>) -> Option<Bytes> {
        self.1.insert(key, value.into())
    }
    pub fn int_iter(&self) -> impl Iterator<Item = (u16, &Bytes)> {
        self.1.iter().map(|(&key, value)| (key, value))
    }
    pub fn remove_int(&mut self, key: u16) -> Option<Bytes> {
        self.1.remove(&key)
    }
    pub fn len(&self) -> usize {
        self.0.len() + self.1.len()
    }

    pub fn clear(&mut self) {
        self.0.clear();
        self.1.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty() && self.1.is_empty()
    }
}

/// Iterates string-keyed entries. Use `int_iter` for native TTHeader integer keys.
impl IntoIterator for Metadata {
    type Item = (Bytes, Bytes);
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcMode {
    Unary,
    ClientStreaming,
    ServerStreaming,
    Bidirectional,
}

/// The homogeneous message format of an RPC, independent of its transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadKind {
    Thrift,
    Protobuf,
}

/// A server method selected from a finite registered method table.
///
/// These names are suitable for route metric dimensions. Unresolved calls
/// retain their input names on the context, but have no `RpcRoute`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RpcRoute<'a> {
    pub service: &'a str,
    pub method: &'a str,
    pub payload: PayloadKind,
    /// Raw methods may be registered without a typed streaming mode.
    pub mode: Option<RpcMode>,
}

#[derive(Clone, Debug, Default)]
pub struct StreamStatus {
    pub error: Option<RpcError>,
    pub header: Metadata,
    pub trailer: Metadata,
    pub sent_messages: u64,
    pub sent_bytes: u64,
    pub received_messages: u64,
    pub received_bytes: u64,
}

/// Implemented by the native transport's existing call state.
///
/// The shared context retains this authority through the original Arc. Drivers
/// commit terminal status and own their queues, cancellation and managed tasks.
#[doc(hidden)]
pub trait ContextDriver: Send + Sync + 'static {
    fn middleware(&self) -> Option<super::ServerMiddleware> {
        None
    }
    fn hooks(&self) -> Option<&super::CallHooks> {
        None
    }
    fn install_hooks(&self, _hooks: super::CallHooks) -> bool {
        false
    }
    fn install_scope(&self, _scope: Arc<dyn super::StreamScope>) -> bool {
        false
    }
    fn id(&self) -> u64;
    fn service(&self) -> &str;
    fn method(&self) -> &str;
    fn role(&self) -> Role;
    fn mode(&self) -> Option<RpcMode>;
    fn payload(&self) -> Option<PayloadKind> {
        None
    }
    fn route(&self) -> Option<RpcRoute<'_>> {
        None
    }
    fn deadline(&self) -> Option<Instant>;
    fn metadata(&self) -> Metadata;
    fn response_headers(self: Arc<Self>, metadata: Metadata) -> RpcResult<()>;
    fn response_trailers(self: Arc<Self>, metadata: Metadata) -> RpcResult<()>;
    fn status(&self) -> Option<StreamStatus>;
    fn is_finished(&self) -> bool {
        self.status().is_some()
    }
    fn cancellation(&self) -> Option<RpcError>;
    fn cancel(self: Arc<Self>, error: RpcError);
    fn cancelled(self: Arc<Self>) -> BoxFuture<'static, RpcError>;
    /// Enter the native context/metainfo scope and invoke operation exactly once.
    fn enter(self: Arc<Self>, operation: &mut dyn FnMut());
    /// True only after all currently installed native and user scopes entered.
    fn is_entered(&self) -> bool {
        false
    }
    fn with_extensions(&self, operation: &mut dyn FnMut(&mut Extensions));
    /// Start one task owned by the native RPC lifecycle.
    fn spawn_scoped(
        self: Arc<Self>,
        operation: BoxFuture<'static, ()>,
    ) -> BoxFuture<'static, RpcResult<()>>;

    fn reclaimed(self: Arc<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}

tokio::task_local! { static STREAM_CONTEXT: StreamContext; }

/// A protocol-independent view of the native call's shared authority.
#[derive(Clone)]
pub struct StreamContext {
    pub(super) driver: Arc<dyn ContextDriver>,
}

impl std::fmt::Debug for StreamContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamContext")
            .field("id", &self.id())
            .field("service", &self.service())
            .field("method", &self.method())
            .field("mode", &self.mode())
            .finish_non_exhaustive()
    }
}

impl StreamContext {
    /// Install one framework scope for handler/body polls and managed tasks.
    /// Implementations invoke the supplied operation exactly once.
    pub fn set_scope(&self, scope: Arc<dyn super::StreamScope>) -> bool {
        self.driver.install_scope(scope)
    }
    #[doc(hidden)]
    pub async fn reclaimed(&self) {
        self.driver.clone().reclaimed().await;
    }
    #[doc(hidden)]
    pub fn from_driver(driver: Arc<dyn ContextDriver>) -> Self {
        Self { driver }
    }

    pub fn current() -> Option<Self> {
        STREAM_CONTEXT.try_with(Clone::clone).ok()
    }

    pub fn id(&self) -> u64 {
        self.driver.id()
    }

    pub fn service(&self) -> &str {
        self.driver.service()
    }

    pub fn method(&self) -> &str {
        self.driver.method()
    }

    pub fn role(&self) -> Role {
        self.driver.role()
    }

    pub fn mode(&self) -> Option<RpcMode> {
        self.driver.mode()
    }

    /// Returns the native payload when known.
    pub fn payload(&self) -> Option<PayloadKind> {
        self.driver.payload()
    }

    /// Returns a resolved, registered server method. Client contexts and unresolved calls return
    /// `None`; map these to a fixed metric label instead of using arbitrary
    /// `service()`/`method()` input strings.
    pub fn route(&self) -> Option<RpcRoute<'_>> {
        self.driver.route()
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.driver.deadline()
    }

    pub fn metadata(&self) -> Metadata {
        self.driver.metadata()
    }

    pub fn response_headers(&self, metadata: Metadata) -> RpcResult<()> {
        self.driver.clone().response_headers(metadata)
    }

    pub fn response_trailers(&self, metadata: Metadata) -> RpcResult<()> {
        self.driver.clone().response_trailers(metadata)
    }

    pub fn status(&self) -> Option<StreamStatus> {
        self.driver.status()
    }
    pub fn is_finished(&self) -> bool {
        self.driver.is_finished()
    }

    pub fn cancellation(&self) -> Option<RpcError> {
        self.driver.cancellation()
    }

    pub fn cancel(&self, error: RpcError) {
        self.driver.clone().cancel(error);
    }

    pub fn cancelled(&self) -> BoxFuture<'static, RpcError> {
        self.driver.clone().cancelled()
    }

    pub fn with_extensions<R>(&self, operation: impl FnOnce(&mut Extensions) -> R) -> R {
        let mut operation = Some(operation);
        let mut result = None;
        self.driver.with_extensions(&mut |extensions| {
            result = Some(operation
                .take()
                .expect("context driver invoked operation twice")(
                extensions
            ));
        });
        result.expect("context driver did not invoke operation")
    }

    /// Compare driver identity without cloning the current context or its Arc.
    #[doc(hidden)]
    pub fn is_current_driver(driver: &dyn ContextDriver) -> bool {
        STREAM_CONTEXT
            .try_with(|current| std::ptr::addr_eq(Arc::as_ptr(&current.driver), driver))
            .unwrap_or(false)
    }

    /// Install the common task local without re-entering the native driver.
    #[doc(hidden)]
    pub fn sync_scope_current<R>(&self, operation: impl FnOnce() -> R) -> R {
        if STREAM_CONTEXT
            .try_with(|current| Arc::ptr_eq(&self.driver, &current.driver))
            .unwrap_or(false)
        {
            operation()
        } else {
            STREAM_CONTEXT.sync_scope(self.clone(), operation)
        }
    }

    pub fn sync_scope<R>(&self, operation: impl FnOnce() -> R) -> R {
        if STREAM_CONTEXT
            .try_with(|current| Arc::ptr_eq(&current.driver, &self.driver))
            .unwrap_or(false)
            && self.driver.is_entered()
        {
            return operation();
        }
        let mut operation = Some(operation);
        let mut result = None;
        // Let the native driver inspect the previously entered common context
        // before publishing this one; metainfo-only reentry is not full entry.
        self.driver.clone().enter(&mut || {
            result = Some(
                self.sync_scope_current(
                    operation
                        .take()
                        .expect("context driver invoked operation twice"),
                ),
            );
        });
        result.expect("context driver did not invoke operation")
    }

    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        tokio::pin!(future);
        futures::future::poll_fn(|task| self.sync_scope(|| future.as_mut().poll(task))).await
    }

    /// Starts work owned by this RPC, with the same explicit and task-local context.
    pub fn spawn_scoped<F>(
        &self,
        future: F,
    ) -> impl Future<Output = RpcResult<F::Output>> + Send + 'static
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let cx = self.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task = self.driver.clone().spawn_scoped(Box::pin(async move {
            let result = cx.scope(future).await;
            let _ = sender.send(result);
        }));
        async move {
            task.await?;
            receiver
                .await
                .map_err(|_| RpcError::canceled("managed RPC task ended without a result"))
        }
    }
}
