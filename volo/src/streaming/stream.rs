use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Stream, StreamExt, future::BoxFuture};

use super::{RpcResult, StreamContext};

/// Native encoders stage owned bytes synchronously, then submit without allocating
/// a future per message. `abort_send` releases a pending message and all permits.
#[doc(hidden)]
pub trait ResponseSink<T>: Send + 'static {
    fn prepare(&mut self, value: &T) -> RpcResult<()>;
    fn poll_send(&mut self, task: &mut Context<'_>) -> Poll<RpcResult<()>>;
    fn abort_send(&mut self);
}

/// A typed request stream backed directly by the selected transport's decoder.
pub struct RequestStream<T> {
    context: StreamContext,
    inner: Option<Pin<Box<dyn Stream<Item = RpcResult<T>> + Send + 'static>>>,
    cancelled: BoxFuture<'static, super::RpcError>,
}

impl<T> RequestStream<T> {
    #[doc(hidden)]
    pub fn new(
        context: StreamContext,
        stream: impl Stream<Item = RpcResult<T>> + Send + 'static,
    ) -> Self {
        let cancelled = context.cancelled();
        Self {
            context,
            inner: Some(Box::pin(stream)),
            cancelled,
        }
    }

    pub fn context(&self) -> &StreamContext {
        &self.context
    }

    /// `None` closes the request direction. Responses can still be sent.
    pub async fn recv(&mut self) -> RpcResult<Option<T>> {
        self.next().await.transpose()
    }
}

impl<T> Stream for RequestStream<T> {
    type Item = RpcResult<T>;

    fn poll_next(self: Pin<&mut Self>, task: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(None);
        };
        if let Poll::Ready(error) = this.cancelled.as_mut().poll(task) {
            this.inner = None;
            return Poll::Ready(Some(Err(error)));
        }
        let result = this.context.sync_scope(|| inner.as_mut().poll_next(task));
        if matches!(result, Poll::Ready(None)) {
            this.inner = None;
        }
        result
    }
}

/// A single ordered response producer. The driver owns final status publication.
pub struct ResponseSender<T> {
    context: StreamContext,
    sink: Box<dyn ResponseSink<T>>,
    cancelled: BoxFuture<'static, super::RpcError>,
    cancellation: Option<super::RpcError>,
}

impl<T: 'static> ResponseSender<T> {
    #[doc(hidden)]
    pub fn new(context: StreamContext, sink: impl ResponseSink<T>) -> Self {
        let cancelled = context.cancelled();
        Self {
            context,
            sink: Box::new(sink),
            cancelled,
            cancellation: None,
        }
    }

    pub fn context(&self) -> &StreamContext {
        &self.context
    }

    /// Encodes before returning. The message borrow ends at this call, and the
    /// future holds only native owned send state and the mutable producer borrow.
    pub fn send<'a>(
        &'a mut self,
        value: &T,
    ) -> impl Future<Output = RpcResult<()>> + Send + use<'a, T> {
        let prepared = match self
            .cancellation
            .clone()
            .or_else(|| self.context.cancellation())
        {
            Some(error) => Err(error),
            None if self.context.is_finished() => {
                Err(super::RpcError::canceled("RPC already finished"))
            }
            None => self.context.sync_scope(|| self.sink.prepare(value)),
        };
        let pending = PendingSend(&mut *self.sink);
        let cx = &self.context;
        let cancelled = &mut self.cancelled;
        let cancellation = &mut self.cancellation;
        async move {
            let pending = pending;
            prepared?;
            futures::future::poll_fn(|task| {
                if let Poll::Ready(error) = cancelled.as_mut().poll(task) {
                    *cancellation = Some(error.clone());
                    return Poll::Ready(Err(error));
                }
                cx.sync_scope(|| pending.0.poll_send(task))
            })
            .await
        }
    }
}

// Also runs if the returned future is dropped before its first poll.
struct PendingSend<'a, T: 'static>(&'a mut dyn ResponseSink<T>);
impl<T: 'static> Drop for PendingSend<'_, T> {
    fn drop(&mut self) {
        self.0.abort_send();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use crate::{
        context::{Extensions, Role},
        streaming::{ContextDriver, Metadata, RpcError, RpcMode, StreamStatus},
    };

    // Cancellation can be reported asynchronously before a driver publishes
    // terminal status. The erased sender must tolerate this for every driver.
    struct AsyncCancellation;
    impl ContextDriver for AsyncCancellation {
        fn id(&self) -> u64 {
            1
        }
        fn service(&self) -> &str {
            "test"
        }
        fn method(&self) -> &str {
            "send"
        }
        fn role(&self) -> Role {
            Role::Server
        }
        fn mode(&self) -> Option<RpcMode> {
            None
        }
        fn deadline(&self) -> Option<std::time::Instant> {
            None
        }
        fn metadata(&self) -> Metadata {
            Metadata::default()
        }
        fn response_headers(self: Arc<Self>, _: Metadata) -> RpcResult<()> {
            Ok(())
        }
        fn response_trailers(self: Arc<Self>, _: Metadata) -> RpcResult<()> {
            Ok(())
        }
        fn status(&self) -> Option<StreamStatus> {
            None
        }
        fn cancellation(&self) -> Option<RpcError> {
            None
        }
        fn cancel(self: Arc<Self>, _: RpcError) {}
        fn cancelled(self: Arc<Self>) -> BoxFuture<'static, RpcError> {
            Box::pin(async { RpcError::canceled("asynchronous cancellation") })
        }
        fn enter(self: Arc<Self>, operation: &mut dyn FnMut()) {
            operation();
        }
        fn with_extensions(&self, operation: &mut dyn FnMut(&mut Extensions)) {
            operation(&mut Extensions::default());
        }
        fn spawn_scoped(
            self: Arc<Self>,
            operation: BoxFuture<'static, ()>,
        ) -> BoxFuture<'static, RpcResult<()>> {
            Box::pin(async move {
                operation.await;
                Ok(())
            })
        }
    }
    struct Sink(Arc<AtomicUsize>);
    impl ResponseSink<()> for Sink {
        fn prepare(&mut self, _: &()) -> RpcResult<()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        fn poll_send(&mut self, _: &mut Context<'_>) -> Poll<RpcResult<()>> {
            Poll::Pending
        }
        fn abort_send(&mut self) {}
    }

    #[tokio::test]
    async fn repeated_sends_cache_asynchronous_cancellation() {
        let prepared = Arc::new(AtomicUsize::new(0));
        let mut sender = ResponseSender::new(
            StreamContext::from_driver(Arc::new(AsyncCancellation)),
            Sink(prepared.clone()),
        );
        let first = sender.send(&()).await.unwrap_err().to_string();
        assert_eq!(sender.send(&()).await.unwrap_err().to_string(), first);
        assert_eq!(prepared.load(Ordering::Relaxed), 1);
    }
}
