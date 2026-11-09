//! One authoritative MetaInfo across independently polled handler/body futures.

use std::{
    cell::RefCell,
    future::Future,
    sync::Arc,
    task::{Context, Poll},
};

use parking_lot::{Condvar, Mutex};
use pin_project::pin_project;

tokio::task_local! { static CURRENT: MetaInfoScope; }

/// Shares one metainfo value between the stages of a logical RPC.
///
/// Each poll lends it to the existing `metainfo::METAINFO` task-local. No mutex
/// is held while running application code or while a future is pending. Nested
/// RPC scopes temporarily return their parent's value, allowing explicit access
/// to that parent without creating divergent metadata copies.
#[derive(Clone, Default)]
pub struct MetaInfoScope(Arc<Shared>);

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    ready: Condvar,
}

struct State(Option<metainfo::MetaInfo>);
impl Default for State {
    fn default() -> Self {
        Self(Some(metainfo::MetaInfo::default()))
    }
}

impl std::fmt::Debug for MetaInfoScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetaInfoScope").finish_non_exhaustive()
    }
}

impl MetaInfoScope {
    pub fn new(info: metainfo::MetaInfo) -> Self {
        Self(Arc::new(Shared {
            state: Mutex::new(State(Some(info))),
            ready: Condvar::new(),
        }))
    }

    /// Derive a child using metainfo's own forward/backward sharing semantics.
    /// If no task-local is installed, create an empty scope.
    pub fn capture() -> Self {
        Self::new(
            metainfo::METAINFO
                .try_with(|slot| {
                    let (parent, child) = std::mem::take(&mut *slot.borrow_mut()).derive();
                    *slot.borrow_mut() = parent;
                    child
                })
                .unwrap_or_default(),
        )
    }

    fn is_current(&self) -> bool {
        CURRENT
            .try_with(|current| Arc::ptr_eq(&self.0, &current.0))
            .unwrap_or(false)
    }

    pub fn with<R>(&self, read: impl FnOnce(&metainfo::MetaInfo) -> R) -> R {
        if self.is_current() {
            return metainfo::METAINFO.with(|slot| read(&slot.borrow()));
        }
        let mut state = self.0.state.lock();
        while state.0.is_none() {
            self.0.ready.wait(&mut state);
        }
        read(state.0.as_ref().expect("available metainfo"))
    }

    pub fn update<R>(&self, update: impl FnOnce(&mut metainfo::MetaInfo) -> R) -> R {
        if self.is_current() {
            return metainfo::METAINFO.with(|slot| update(&mut slot.borrow_mut()));
        }
        let mut state = self.0.state.lock();
        while state.0.is_none() {
            self.0.ready.wait(&mut state);
        }
        update(state.0.as_mut().expect("available metainfo"))
    }

    fn take(&self) -> metainfo::MetaInfo {
        let mut state = self.0.state.lock();
        while state.0.is_none() {
            self.0.ready.wait(&mut state);
        }
        state.0.take().expect("available metainfo")
    }

    fn put(&self, info: metainfo::MetaInfo) {
        self.0.state.lock().0 = Some(info);
        self.0.ready.notify_all();
    }

    pub fn sync_scope<R>(&self, operation: impl FnOnce() -> R) -> R {
        if self.is_current() {
            return operation();
        }
        let parent = CURRENT.try_with(Clone::clone).ok();
        if let Some(parent) = &parent {
            metainfo::METAINFO.with(|slot| parent.put(std::mem::take(&mut *slot.borrow_mut())));
        }
        let _resume = ResumeParent(parent);
        let info = self.take();
        CURRENT.sync_scope(self.clone(), || {
            metainfo::METAINFO.sync_scope(RefCell::new(info), || {
                let _return_owner = ReturnOwner(self.clone());
                operation()
            })
        })
    }

    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        Scope {
            owner: self.clone(),
            future,
        }
        .await
    }
}

struct ReturnOwner(MetaInfoScope);
impl Drop for ReturnOwner {
    fn drop(&mut self) {
        metainfo::METAINFO.with(|slot| self.0.put(std::mem::take(&mut *slot.borrow_mut())));
    }
}

struct ResumeParent(Option<MetaInfoScope>);
impl Drop for ResumeParent {
    fn drop(&mut self) {
        if let Some(parent) = &self.0 {
            let info = parent.take();
            metainfo::METAINFO.with(|slot| *slot.borrow_mut() = info);
        }
    }
}

#[pin_project]
struct Scope<F> {
    owner: MetaInfoScope,
    #[pin]
    future: F,
}

impl<F: Future> Future for Scope<F> {
    type Output = F::Output;
    fn poll(self: std::pin::Pin<&mut Self>, task: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        this.owner.sync_scope(|| this.future.poll(task))
    }
}

#[cfg(test)]
mod tests {
    use metainfo::{Forward, METAINFO};

    use super::*;

    #[tokio::test]
    async fn nested_scopes_keep_one_value_across_pending_and_explicit_parent_updates() {
        let parent = MetaInfoScope::default();
        let child = MetaInfoScope::default();
        parent
            .scope(async {
                METAINFO.with(|slot| slot.borrow_mut().set_persistent("key", "parent"));
                child
                    .scope(async {
                        parent.update(|info| info.set_persistent("key", "updated"));
                        child.update(|info| info.set_persistent("key", "child"));
                        tokio::task::yield_now().await;
                        assert_eq!(
                            parent.with(|info| info.get_persistent("key")).as_deref(),
                            Some("updated")
                        );
                    })
                    .await;
                assert_eq!(
                    METAINFO
                        .with(|slot| slot.borrow().get_persistent("key"))
                        .as_deref(),
                    Some("updated")
                );
            })
            .await;
        assert_eq!(
            child.with(|info| info.get_persistent("key")).as_deref(),
            Some("child")
        );
    }

    #[test]
    fn unwinding_returns_the_value_and_restores_the_parent() {
        let owner = MetaInfoScope::default();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            owner.sync_scope(|| {
                METAINFO.with(|slot| slot.borrow_mut().set_transient("key", "retained"));
                panic!("fixture panic");
            })
        }));
        assert!(result.is_err());
        assert_eq!(
            owner.with(|info| info.get_transient("key")).as_deref(),
            Some("retained")
        );
    }
}
