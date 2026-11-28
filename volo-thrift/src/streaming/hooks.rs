use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use super::{Metadata, StreamContext, StreamError, StreamIoError, StreamStatus};

pub trait StreamHook: Send + Sync + 'static {
    fn on_open(&self, _cx: &mut StreamContext) -> Result<(), StreamError> {
        Ok(())
    }
    fn on_meta(&self, _cx: &mut StreamContext, _meta: &Metadata) {}
    fn on_header(&self, _cx: &mut StreamContext, _header: &Metadata) {}
    fn on_send(&self, _cx: &StreamContext, _size: usize) {}
    fn on_recv(&self, _cx: &StreamContext, _size: usize) {}
    fn on_sent_header(&self, _cx: &StreamContext, _metadata: &Metadata) {}
    fn on_trailer(&self, _cx: &StreamContext, _metadata: &Metadata) {}
    fn on_sent_trailer(&self, _cx: &StreamContext) {}
    fn on_io_error(&self, _cx: &StreamContext, _event: &StreamIoError) {}
    fn on_finish(&self, _cx: &StreamContext, _status: &StreamStatus) {}
}

/// Static empty hook path: no allocations, event clocks or trait calls.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopHooks;
impl StreamHook for NoopHooks {}

struct Registered {
    hook: Arc<dyn StreamHook>,
    disabled: AtomicBool,
}
#[derive(Clone, Default)]
pub struct HookChain {
    hooks: Arc<[Registered]>,
    common: bool,
}

impl std::fmt::Debug for HookChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookChain")
            .field("len", &self.hooks.len())
            .finish()
    }
}

impl HookChain {
    pub(crate) fn for_call(&self) -> Self {
        Self {
            hooks: self
                .hooks
                .iter()
                .map(|entry| Registered {
                    hook: entry.hook.clone(),
                    disabled: AtomicBool::new(false),
                })
                .collect(),
            common: self.common,
        }
    }
    pub fn new(hooks: impl IntoIterator<Item = Arc<dyn StreamHook>>) -> Self {
        Self {
            hooks: hooks
                .into_iter()
                .map(|hook| Registered {
                    hook,
                    disabled: AtomicBool::new(false),
                })
                .collect(),
            common: false,
        }
    }
    /// Prepends an observer while retaining the order and disabled state of existing hooks.
    pub fn push_front(&mut self, hook: Arc<dyn StreamHook>) {
        self.insert(0, hook);
    }
    pub fn push(&mut self, hook: Arc<dyn StreamHook>) {
        self.insert(self.hooks.len(), hook);
    }
    fn insert(&mut self, index: usize, hook: Arc<dyn StreamHook>) {
        let mut hooks = self
            .hooks
            .iter()
            .map(|entry| Registered {
                hook: entry.hook.clone(),
                disabled: AtomicBool::new(entry.disabled.load(Ordering::Relaxed)),
            })
            .collect::<Vec<_>>();
        hooks.insert(
            index,
            Registered {
                hook,
                disabled: AtomicBool::new(false),
            },
        );
        self.hooks = hooks.into();
    }
    pub(crate) fn with_common(mut self) -> Self {
        self.push(Arc::new(CommonHook));
        self.common = true;
        self
    }
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
    fn invoke(
        &self,
        mut call: impl FnMut(&dyn StreamHook) -> Result<(), StreamError>,
    ) -> Result<(), StreamError> {
        let mut result = Ok(());
        for entry in self.hooks.iter() {
            if entry.disabled.load(Ordering::Relaxed) {
                continue;
            }
            match catch_unwind(AssertUnwindSafe(|| call(&*entry.hook))) {
                Ok(Err(error)) if result.is_ok() => result = Err(error),
                Ok(_) => {}
                Err(_) => {
                    entry.disabled.store(true, Ordering::Relaxed);
                    tracing::error!("stream observer panicked and was disabled for this RPC");
                }
            }
        }
        result
    }
}
impl StreamHook for HookChain {
    fn on_open(&self, cx: &mut StreamContext) -> Result<(), StreamError> {
        let native = self.invoke(|hook| hook.on_open(cx)).and_then(|()| {
            if self
                .hooks
                .iter()
                .any(|entry| entry.disabled.load(Ordering::Relaxed))
            {
                Err(StreamError::Protocol("stream open hook panicked".into()))
            } else {
                Ok(())
            }
        });
        let common = if self.common {
            cx.shared_context().open_hooks().map_err(StreamError::from)
        } else {
            Ok(())
        };
        native.and(common)
    }
    fn on_meta(&self, cx: &mut StreamContext, meta: &Metadata) {
        if let Err(error) = self.invoke(|h| {
            h.on_meta(cx, meta);
            Ok(())
        }) {
            cx.cancel(error);
        }
    }
    fn on_header(&self, cx: &mut StreamContext, meta: &Metadata) {
        if let Err(error) = self.invoke(|h| {
            h.on_header(cx, meta);
            Ok(())
        }) {
            cx.cancel(error);
        }
    }
    fn on_send(&self, cx: &StreamContext, size: usize) {
        if let Err(error) = self.invoke(|h| {
            h.on_send(cx, size);
            Ok(())
        }) {
            cx.cancel(error);
        }
    }
    fn on_recv(&self, cx: &StreamContext, size: usize) {
        if let Err(error) = self.invoke(|h| {
            h.on_recv(cx, size);
            Ok(())
        }) {
            cx.cancel(error);
        }
    }
    fn on_sent_header(&self, cx: &StreamContext, metadata: &Metadata) {
        if let Err(error) = self.invoke(|h| {
            h.on_sent_header(cx, metadata);
            Ok(())
        }) {
            cx.cancel(error);
        }
    }
    fn on_trailer(&self, cx: &StreamContext, metadata: &Metadata) {
        if let Err(error) = self.invoke(|h| {
            h.on_trailer(cx, metadata);
            Ok(())
        }) {
            cx.cancel(error);
        }
    }
    fn on_sent_trailer(&self, cx: &StreamContext) {
        if let Err(error) = self.invoke(|h| {
            h.on_sent_trailer(cx);
            Ok(())
        }) {
            cx.cancel(error);
        }
    }
    fn on_io_error(&self, cx: &StreamContext, event: &StreamIoError) {
        if let Err(error) = self.invoke(|h| {
            h.on_io_error(cx, event);
            Ok(())
        }) {
            cx.cancel(error);
        }
    }
    fn on_finish(&self, cx: &StreamContext, status: &StreamStatus) {
        // Terminal cleanup is independent for every observer. A user hook panic
        // must not prevent later metrics/tracing hooks from closing their state.
        for entry in self.hooks.iter() {
            if entry.disabled.load(Ordering::Relaxed) {
                continue;
            }
            if catch_unwind(AssertUnwindSafe(|| entry.hook.on_finish(cx, status))).is_err() {
                entry.disabled.store(true, Ordering::Relaxed);
            }
        }
    }
}

/// A subscription in the existing native chain; it owns no protocol state.
struct CommonHook;
impl StreamHook for CommonHook {
    fn on_header(&self, cx: &mut StreamContext, metadata: &Metadata) {
        cx.observe_header(volo::streaming::StreamDirection::Recv, metadata);
    }
    fn on_meta(&self, cx: &mut StreamContext, metadata: &Metadata) {
        cx.observe_meta(volo::streaming::StreamDirection::Recv, metadata);
    }
    fn on_sent_header(&self, cx: &StreamContext, metadata: &Metadata) {
        cx.observe_header(volo::streaming::StreamDirection::Send, metadata);
    }
    fn on_send(&self, cx: &StreamContext, size: usize) {
        cx.observe_send(size);
    }
    fn on_recv(&self, cx: &StreamContext, size: usize) {
        cx.observe_recv(size);
    }
    fn on_trailer(&self, cx: &StreamContext, metadata: &Metadata) {
        cx.observe_trailer(volo::streaming::StreamDirection::Recv, metadata);
        cx.observe_half_close(volo::streaming::StreamDirection::Recv);
    }
    fn on_sent_trailer(&self, cx: &StreamContext) {
        cx.observe_sent_trailer();
    }
    fn on_io_error(&self, cx: &StreamContext, event: &StreamIoError) {
        cx.observe_io_error(&event.error);
    }
    fn on_finish(&self, cx: &StreamContext, _: &StreamStatus) {
        if let Some(hooks) = cx.inner.common_hooks.get() {
            hooks.close(&cx.shared_context());
        }
    }
}

#[cfg(test)]
mod review_tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    #[test]
    fn observer_panic_is_local_to_one_call_and_other_observers_continue() {
        struct PanicOnce(AtomicUsize);
        impl StreamHook for PanicOnce {
            fn on_send(&self, _: &StreamContext, _: usize) {
                assert_ne!(self.0.fetch_add(1, Ordering::Relaxed), 0, "fixture panic");
            }
        }
        struct Count(AtomicUsize);
        impl StreamHook for Count {
            fn on_send(&self, _: &StreamContext, _: usize) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let panicking = Arc::new(PanicOnce(AtomicUsize::new(0)));
        let healthy = Arc::new(Count(AtomicUsize::new(0)));
        let registered =
            HookChain::new([panicking.clone() as Arc<dyn StreamHook>, healthy.clone()]);
        let cx = StreamContext::new(1, "S", "M", volo::context::Role::Client);
        let first = registered.for_call();
        first.on_send(&cx, 1);
        first.on_send(&cx, 1);
        registered.for_call().on_send(&cx, 1);
        assert_eq!(healthy.0.load(Ordering::Relaxed), 3);
        assert_eq!(panicking.0.load(Ordering::Relaxed), 2);
        assert!(cx.cancellation().is_none());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    struct Panics;
    impl StreamHook for Panics {
        fn on_finish(&self, _: &StreamContext, _: &StreamStatus) {
            panic!("user hook panic");
        }
    }
    struct Observes(Arc<AtomicUsize>);
    impl StreamHook for Observes {
        fn on_finish(&self, _: &StreamContext, _: &StreamStatus) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    #[test]
    fn a_panicking_finish_hook_does_not_skip_remaining_cleanup() {
        let observed = Arc::new(AtomicUsize::new(0));
        let hooks = HookChain::new([
            Arc::new(Panics) as Arc<dyn StreamHook>,
            Arc::new(Observes(observed.clone())),
        ]);
        let cx = StreamContext::new(1, "S", "M", volo::context::Role::Client);
        hooks.on_finish(&cx, &StreamStatus::default());
        assert_eq!(observed.load(Ordering::Relaxed), 1);
    }
}

#[cfg(test)]
mod front_order_test {
    use super::*;
    #[test]
    fn prepend_preserves_existing_hook_order() {
        struct Tag(u8, Arc<std::sync::Mutex<Vec<u8>>>);
        impl StreamHook for Tag {
            fn on_open(&self, _: &mut StreamContext) -> Result<(), StreamError> {
                self.1.lock().unwrap().push(self.0);
                Ok(())
            }
        }
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut hooks = HookChain::default();
        hooks.push(Arc::new(Tag(2, events.clone())));
        hooks.push(Arc::new(Tag(3, events.clone())));
        hooks.push_front(Arc::new(Tag(1, events.clone())));
        let mut cx = StreamContext::new(1, "Test", "Bidi", volo::context::Role::Server);
        hooks.on_open(&mut cx).unwrap();
        assert_eq!(*events.lock().unwrap(), vec![1, 2, 3]);
    }
}
