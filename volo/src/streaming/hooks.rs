use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use super::{Metadata, RpcError, RpcResult, StreamContext, StreamStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamDirection {
    Send,
    Recv,
}

/// Observes one native RPC. A hook never owns its terminal state.
pub trait StreamHook: Send + Sync + 'static {
    fn on_open(&self, _cx: &StreamContext) -> RpcResult<()> {
        Ok(())
    }
    fn on_header(&self, _cx: &StreamContext, _direction: StreamDirection, _metadata: &Metadata) {}
    fn on_meta(&self, _cx: &StreamContext, _direction: StreamDirection, _metadata: &Metadata) {}
    fn on_send(&self, _cx: &StreamContext, _size: usize) {}
    fn on_recv(&self, _cx: &StreamContext, _size: usize) {}
    fn on_flow_control(&self, _cx: &StreamContext, _wait: Duration) {}
    fn on_half_close(&self, _cx: &StreamContext, _direction: StreamDirection) {}
    fn on_trailer(&self, _cx: &StreamContext, _direction: StreamDirection, _metadata: &Metadata) {}
    fn on_io_error(&self, _cx: &StreamContext, _error: &RpcError) {}
    fn on_finish(&self, _cx: &StreamContext, _status: &StreamStatus) {}
}

pub trait StreamHookFactory: Send + Sync + 'static {
    fn enabled(&self, _cx: &StreamContext) -> bool {
        true
    }
    fn create(&self, cx: &StreamContext) -> Arc<dyn StreamHook>;
}
impl<F, H> StreamHookFactory for F
where
    F: Fn(&StreamContext) -> H + Send + Sync + 'static,
    H: StreamHook,
{
    fn create(&self, cx: &StreamContext) -> Arc<dyn StreamHook> {
        Arc::new(self(cx))
    }
}

struct Entry {
    hook: Arc<dyn StreamHook>,
    disabled: AtomicBool,
}
struct Hooks {
    entries: Vec<Entry>,
    events: AtomicUsize,
    halves: AtomicUsize,
}
const CLOSED: usize = 1 << (usize::BITS - 1);

/// Native contexts retain this observer gate; finish reads their committed status.
#[doc(hidden)]
#[derive(Clone)]
pub struct CallHooks(Arc<Hooks>);
impl std::fmt::Debug for CallHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CallHooks { .. }")
    }
}
impl CallHooks {
    pub(super) fn create(
        factories: &[Arc<dyn StreamHookFactory>],
        cx: &StreamContext,
    ) -> (Option<Self>, RpcResult<()>) {
        let mut entries = Vec::new();
        let mut result = Ok(());
        for factory in factories {
            match catch_unwind(AssertUnwindSafe(|| {
                cx.sync_scope(|| factory.enabled(cx).then(|| factory.create(cx)))
            })) {
                Ok(Some(hook)) => entries.push(Entry {
                    hook,
                    disabled: AtomicBool::new(false),
                }),
                Ok(None) => {}
                Err(_) => {
                    result = Err(RpcError::Protocol("stream hook factory panicked".into()));
                }
            }
        }
        (
            (!entries.is_empty()).then(|| {
                Self(Arc::new(Hooks {
                    entries,
                    // Reserve opening before publication to the native owner.
                    events: AtomicUsize::new(1),
                    halves: AtomicUsize::new(0),
                }))
            }),
            result,
        )
    }
    fn admit(&self, cx: &StreamContext) -> Option<Event> {
        let mut value = self.0.events.load(Ordering::Acquire);
        loop {
            if value & CLOSED != 0 {
                return None;
            }
            match self.0.events.compare_exchange_weak(
                value,
                value + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => value = actual,
            }
        }
        Some(Event {
            hooks: self.clone(),
            cx: cx.clone(),
        })
    }
    /// Retained for native adapters; all events share the same finish barrier,
    /// including opening headers emitted outside a native message callback.
    pub fn use_native_events(&self) {}
    fn each(&self, cx: &StreamContext, callback: impl FnMut(&dyn StreamHook)) {
        self.guarded(cx, callback);
    }
    fn guarded(&self, cx: &StreamContext, callback: impl FnMut(&dyn StreamHook)) {
        let Some(_event) = self.admit(cx) else {
            return;
        };
        self.dispatch(cx, callback);
    }
    fn dispatch(&self, cx: &StreamContext, mut callback: impl FnMut(&dyn StreamHook)) {
        cx.sync_scope(|| {
            for entry in &self.0.entries {
                if entry.disabled.load(Ordering::Relaxed) {
                    continue;
                }
                if catch_unwind(AssertUnwindSafe(|| callback(&*entry.hook))).is_err() {
                    entry.disabled.store(true, Ordering::Relaxed);
                    tracing::error!("stream observer panicked and was disabled");
                }
            }
        });
    }
    pub(super) fn open(&self, cx: &StreamContext) -> RpcResult<()> {
        let _opening = Event {
            hooks: self.clone(),
            cx: cx.clone(),
        };
        let mut result = Ok(());
        self.dispatch(cx, |hook| {
            if let Err(error) = hook.on_open(cx) {
                if result.is_ok() {
                    result = Err(error);
                }
            }
        });
        // An open callback panic is an admission failure, while other hooks still
        // receive open/finish so tracing and metrics can close their state.
        if self
            .0
            .entries
            .iter()
            .any(|entry| entry.disabled.load(Ordering::Relaxed))
            && result.is_ok()
        {
            result = Err(RpcError::Protocol("stream open hook panicked".into()));
        }
        result
    }
    pub fn header(&self, cx: &StreamContext, direction: StreamDirection, metadata: &Metadata) {
        self.each(cx, |h| h.on_header(cx, direction, metadata));
    }
    pub fn meta(&self, cx: &StreamContext, direction: StreamDirection, metadata: &Metadata) {
        self.each(cx, |h| h.on_meta(cx, direction, metadata));
    }
    pub fn send(&self, cx: &StreamContext, size: usize) {
        self.each(cx, |h| h.on_send(cx, size));
    }
    pub fn recv(&self, cx: &StreamContext, size: usize) {
        self.each(cx, |h| h.on_recv(cx, size));
    }
    pub fn flow_control(&self, cx: &StreamContext, wait: Duration) {
        self.each(cx, |h| h.on_flow_control(cx, wait));
    }
    pub fn half_close(&self, cx: &StreamContext, direction: StreamDirection) {
        let bit = match direction {
            StreamDirection::Send => 1,
            StreamDirection::Recv => 2,
        };
        if self.0.halves.fetch_or(bit, Ordering::AcqRel) & bit == 0 {
            self.each(cx, |h| h.on_half_close(cx, direction));
        }
    }
    pub fn trailer(&self, cx: &StreamContext, direction: StreamDirection, metadata: &Metadata) {
        self.each(cx, |h| h.on_trailer(cx, direction, metadata));
    }
    pub fn io_error(&self, cx: &StreamContext, error: &RpcError) {
        self.each(cx, |h| h.on_io_error(cx, error));
    }
    pub fn close(&self, cx: &StreamContext) {
        let previous = self.0.events.fetch_or(CLOSED, Ordering::AcqRel);
        if previous == 0 {
            self.finish(cx);
        }
    }
    fn finish(&self, cx: &StreamContext) {
        let Some(status) = cx.status() else {
            tracing::error!("native owner closed stream hooks before committing status");
            return;
        };
        for entry in &self.0.entries {
            if entry.disabled.load(Ordering::Relaxed) {
                continue;
            }
            if catch_unwind(AssertUnwindSafe(|| {
                cx.sync_scope(|| entry.hook.on_finish(cx, &status))
            }))
            .is_err()
            {
                entry.disabled.store(true, Ordering::Relaxed);
                tracing::error!("stream finish observer panicked");
            }
        }
    }
}
struct Event {
    hooks: CallHooks,
    cx: StreamContext,
}
impl Drop for Event {
    fn drop(&mut self) {
        if self.hooks.0.events.fetch_sub(1, Ordering::AcqRel) == (CLOSED | 1) {
            self.hooks.finish(&self.cx);
        }
    }
}
