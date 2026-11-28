use std::{future::Future, pin::Pin, sync::Arc};

use futures::future::BoxFuture;

use super::{CallHooks, RpcError, RpcResult, StreamContext, StreamHookFactory};
use crate::Service;

/// Enter framework task locals for one synchronous operation (usually a poll).
pub trait StreamScope: Send + Sync + 'static {
    fn enter(&self, cx: &StreamContext, operation: &mut dyn FnMut());
}

#[derive(Clone)]
#[doc(hidden)]
pub struct SharedScope(pub Arc<dyn StreamScope>);
impl SharedScope {
    pub fn enter<R>(&self, cx: &StreamContext, operation: impl FnOnce() -> R) -> R {
        let mut operation = Some(operation);
        let mut result = None;
        self.0.enter(cx, &mut || {
            result = Some(operation
                .take()
                .expect("StreamScope invoked operation twice")(
            ))
        });
        result.expect("StreamScope did not invoke operation")
    }
}
impl std::fmt::Debug for SharedScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedScope { .. }")
    }
}

/// One invocation of a typed handler. Middleware observes the common context;
/// the original handler retains its concrete request and response types.
pub struct Invocation<'a> {
    future: Pin<&'a mut (dyn Future<Output = RpcResult<()>> + Send)>,
}

trait Invoke: Send + Sync {
    fn call<'call, 'invocation: 'call>(
        &'call self,
        cx: &'call mut StreamContext,
        request: Invocation<'invocation>,
    ) -> BoxFuture<'call, RpcResult<()>>;
}

struct Erased<S>(S);
impl<S> Invoke for Erased<S>
where
    S: for<'a> Service<StreamContext, Invocation<'a>, Response = (), Error = RpcError>
        + Send
        + Sync,
{
    fn call<'call, 'invocation: 'call>(
        &'call self,
        cx: &'call mut StreamContext,
        request: Invocation<'invocation>,
    ) -> BoxFuture<'call, RpcResult<()>> {
        Box::pin(self.0.call(cx, request))
    }
}

/// A complete invocation layer stack erased at the server preparation boundary.
#[derive(Clone)]
pub struct InvocationService(Arc<dyn Invoke>);

impl InvocationService {
    pub fn new<S>(service: S) -> Self
    where
        S: for<'a> Service<StreamContext, Invocation<'a>, Response = (), Error = RpcError>
            + Send
            + Sync
            + 'static,
    {
        Self(Arc::new(Erased(service)))
    }
}

impl<'a> Service<StreamContext, Invocation<'a>> for InvocationService {
    type Response = ();
    type Error = RpcError;
    async fn call(&self, cx: &mut StreamContext, request: Invocation<'a>) -> RpcResult<()> {
        self.0.call(cx, request).await
    }
}

/// The innermost service of a global server layer stack.
#[derive(Clone, Copy, Debug, Default)]
pub struct InvocationHandler;
impl Service<StreamContext, Invocation<'_>> for InvocationHandler {
    type Response = ();
    type Error = RpcError;
    async fn call(&self, _: &mut StreamContext, request: Invocation<'_>) -> RpcResult<()> {
        request.future.await
    }
}
impl Default for InvocationService {
    fn default() -> Self {
        Self::new(InvocationHandler)
    }
}

/// Server configuration captured by a native call at admission. This contains
/// no per-call terminal state and never changes message or wire ownership.
#[derive(Clone)]
#[doc(hidden)]
pub struct ServerMiddleware(Arc<Settings>);
struct Settings {
    service: Option<InvocationService>,
    hooks: Vec<Arc<dyn StreamHookFactory>>,
}

impl std::fmt::Debug for ServerMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerMiddleware { .. }")
    }
}

tokio::task_local! { static CURRENT: ServerMiddleware; }

impl ServerMiddleware {
    pub fn new(service: Option<InvocationService>, hooks: Vec<Arc<dyn StreamHookFactory>>) -> Self {
        Self(Arc::new(Settings { service, hooks }))
    }
    pub fn has_hooks(&self) -> bool {
        !self.0.hooks.is_empty()
    }
    pub fn current() -> Option<Self> {
        CURRENT.try_with(Clone::clone).ok()
    }
    pub async fn scope_optional<F: Future>(middleware: Option<Self>, future: F) -> F::Output {
        match middleware {
            Some(middleware) => CURRENT.scope(middleware, future).await,
            None => future.await,
        }
    }
    fn invoke<'a, T: Send + 'a, F: Future<Output = RpcResult<T>> + Send + 'a>(
        &'a self,
        cx: &'a StreamContext,
        future: F,
    ) -> impl Future<Output = RpcResult<T>> + Send + 'a {
        // Allocate before constructing the scoped middleware future. Keeping the
        // concrete handler inline compounds its state through every scope poll.
        // This is one RPC boundary; message send/recv paths do not use it.
        let future = Box::pin(future);
        async move {
            let Some(service) = &self.0.service else {
                return future.await;
            };
            // The typed result stays in this invocation future. Middleware cannot retain
            // the borrowed invocation or require an Any/Arc/queue for the response.
            let mut result = None;
            {
                let operation = async {
                    result = Some(future.await?);
                    Ok(())
                };
                tokio::pin!(operation);
                service
                    .call(
                        &mut cx.clone(),
                        Invocation {
                            future: operation.as_mut(),
                        },
                    )
                    .await?;
            }
            result.ok_or_else(|| {
                RpcError::Protocol(
                    "server layer returned success without invoking its inner service".into(),
                )
            })
        }
    }
}

impl StreamContext {
    /// Install observers after native request metadata and routing are available.
    #[doc(hidden)]
    pub fn open_hooks(&self) -> RpcResult<()> {
        if self.driver.hooks().is_some() {
            return Ok(());
        }
        let Some(middleware) = self.driver.middleware() else {
            return Ok(());
        };
        let (hooks, creation) = CallHooks::create(&middleware.0.hooks, self);
        creation?;
        let opening = if let Some(hooks) = hooks {
            if !self.driver.install_hooks(hooks.clone()) {
                return Err(RpcError::Protocol(
                    "native call rejected stream hook installation".into(),
                ));
            }
            let result = hooks.open(self);
            if result.is_ok() {
                hooks.header(self, super::StreamDirection::Recv, &self.metadata());
            }
            if self.status().is_some() {
                hooks.close(self);
            }
            result
        } else {
            Ok(())
        };
        opening?;
        self.cancellation().map_or(Ok(()), Err)
    }
    /// Invoke a typed business handler through the server's configured layers.
    /// This boundary runs once per call; stream messages keep their native path.
    #[doc(hidden)]
    pub async fn invoke<T, H, F>(&self, handler: H) -> RpcResult<T>
    where
        T: Send,
        H: FnOnce(StreamContext) -> F + Send,
        F: Future<Output = RpcResult<T>> + Send,
    {
        let middleware = self.driver.middleware();
        match middleware {
            Some(middleware) if middleware.0.service.is_some() => {
                self.scope(middleware.invoke(self, async { handler(self.clone()).await }))
                    .await
            }
            _ => self.scope(async { handler(self.clone()).await }).await,
        }
    }
}
