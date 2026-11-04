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
}

impl std::fmt::Debug for HookChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookChain")
            .field("len", &self.hooks.len())
            .finish()
    }
}

impl HookChain {
    pub fn new(hooks: impl IntoIterator<Item = Arc<dyn StreamHook>>) -> Self {
        Self {
            hooks: hooks
                .into_iter()
                .map(|hook| Registered {
                    hook,
                    disabled: AtomicBool::new(false),
                })
                .collect(),
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
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
    fn invoke(
        &self,
        mut call: impl FnMut(&dyn StreamHook) -> Result<(), StreamError>,
    ) -> Result<(), StreamError> {
        for entry in self.hooks.iter() {
            if entry.disabled.load(Ordering::Relaxed) {
                continue;
            }
            match catch_unwind(AssertUnwindSafe(|| call(&*entry.hook))) {
                Ok(result) => result?,
                Err(_) => {
                    entry.disabled.store(true, Ordering::Relaxed);
                    return Err(StreamError::Protocol("stream hook panicked".into()));
                }
            }
        }
        Ok(())
    }
}
impl StreamHook for HookChain {
    fn on_open(&self, cx: &mut StreamContext) -> Result<(), StreamError> {
        self.invoke(|hook| hook.on_open(cx))
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
