//! Task-isolated metainfo snapshots for the stages of a logical RPC.

use std::{
    cell::RefCell,
    future::Future,
    sync::Arc,
    task::{Context, Poll},
};

use parking_lot::Mutex;
use pin_project::pin_project;
use rustc_hash::FxHashMap;

tokio::task_local! { static CURRENT: MetaInfoScope; }

/// Each task has its own metainfo snapshot, retained across polls and nested
/// scopes. Changes are never implicitly merged between tasks. Return metadata
/// from a child and apply it explicitly if the parent needs the result.
///
/// The mutex only protects moving snapshots in and out of storage; application
/// code never runs under it, and entry never waits for another task's poll.
/// `task_scope` captures the caller's latest snapshot for a managed child and
/// releases the child's storage on exit. Arbitrary tasks start from the initial
/// snapshot; their storage lives until this RPC context is dropped.
///
/// Mutable callbacks must not reenter metadata access: the exclusive borrow is
/// checked by `RefCell`, just like direct use of `metainfo::METAINFO`.
#[derive(Clone)]
pub struct MetaInfoScope(Arc<Mutex<State>>);

struct State {
    initial: Arc<metainfo::MetaInfo>,
    tasks: Snapshots,
}

/// The handler normally owns the only snapshot. Other framework tasks and
/// explicitly spawned children allocate a table only when they actually enter.
#[derive(Default)]
struct Snapshots {
    primary: Option<(TaskKey, Option<metainfo::MetaInfo>)>,
    other: FxHashMap<TaskKey, Option<metainfo::MetaInfo>>,
}
impl Snapshots {
    fn entry(&mut self, key: TaskKey) -> &mut Option<metainfo::MetaInfo> {
        if self.primary.is_none() {
            self.primary = Some((key, self.other.remove(&key).flatten()));
        }
        let (primary, snapshot) = self.primary.as_mut().expect("primary initialized above");
        if *primary == key {
            snapshot
        } else {
            self.other.entry(key).or_default()
        }
    }

    fn remove(&mut self, key: &TaskKey) -> Option<metainfo::MetaInfo> {
        if self
            .primary
            .as_ref()
            .is_some_and(|(primary, _)| primary == key)
        {
            self.primary.take().and_then(|(_, snapshot)| snapshot)
        } else {
            self.other.remove(key).flatten()
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        usize::from(self.primary.is_some()) + self.other.len()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.primary.is_none() && self.other.is_empty()
    }
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TaskKey {
    Async(tokio::task::Id),
    Sync(std::thread::ThreadId),
}
impl TaskKey {
    fn current() -> Self {
        tokio::task::try_id()
            .map(Self::Async)
            .unwrap_or_else(|| Self::Sync(std::thread::current().id()))
    }
}
impl Default for MetaInfoScope {
    fn default() -> Self {
        Self::new(metainfo::MetaInfo::default())
    }
}
impl std::fmt::Debug for MetaInfoScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetaInfoScope").finish_non_exhaustive()
    }
}
impl MetaInfoScope {
    pub fn new(info: metainfo::MetaInfo) -> Self {
        Self(Arc::new(Mutex::new(State {
            initial: Arc::new(info),
            tasks: Snapshots::default(),
        })))
    }
    /// Capture the current task-local using metainfo's forward/backward rules.
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
    #[doc(hidden)]
    pub fn is_current(&self) -> bool {
        CURRENT
            .try_with(|current| Arc::ptr_eq(&self.0, &current.0))
            .unwrap_or(false)
    }
    /// Read the current task's snapshot. The callback may nest reads of this
    /// scope, but must release its borrow before updating or entering another scope.
    pub fn with<R>(&self, read: impl FnOnce(&metainfo::MetaInfo) -> R) -> R {
        self.sync_scope(|| metainfo::METAINFO.with(|slot| read(&slot.borrow())))
    }
    pub fn update<R>(&self, update: impl FnOnce(&mut metainfo::MetaInfo) -> R) -> R {
        self.sync_scope(|| metainfo::METAINFO.with(|slot| update(&mut slot.borrow_mut())))
    }
    fn snapshot(&self) -> metainfo::MetaInfo {
        self.update(|info| {
            let (parent, child) = std::mem::take(info).derive();
            *info = parent;
            child
        })
    }
    fn take(&self) -> metainfo::MetaInfo {
        let mut state = self.0.lock();
        let State { initial, tasks } = &mut *state;
        tasks
            .entry(TaskKey::current())
            .take()
            .unwrap_or_else(|| metainfo::MetaInfo::from(initial.clone()))
    }
    fn put(&self, info: metainfo::MetaInfo) {
        let previous = self.0.lock().tasks.entry(TaskKey::current()).replace(info);
        drop(previous);
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
                let _return_owner = ReturnOwner(self);
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
    /// Capture now, then enter on a different task. Child updates stay isolated.
    /// If polled in the creating task, this behaves like `scope`: it neither
    /// replaces nor removes that task's existing snapshot.
    pub fn task_scope<F: Future>(&self, future: F) -> impl Future<Output = F::Output> + use<F> {
        ChildScope {
            parent: TaskKey::current(),
            snapshot: Some(self.snapshot()),
            future,
            cleanup: SnapshotLease {
                owner: self.clone(),
                key: None,
            },
        }
    }

    /// Seal admission metadata as the RPC's initial snapshot and hand it to a
    /// framework handler task. Subsequent driver tasks see the admitted input;
    /// later handler updates remain isolated. Releases the admission task entry.
    /// Must be called outside this scope, before spawning the returned future.
    #[doc(hidden)]
    pub fn handoff<F: Future>(&self, future: F) -> impl Future<Output = F::Output> + use<F> {
        assert!(
            !self.is_current(),
            "cannot hand off an entered metainfo scope"
        );
        let child = self.task_scope(future);
        let previous = {
            let mut state = self.0.lock();
            let snapshot = state
                .tasks
                .remove(&TaskKey::current())
                .expect("admission snapshot was captured above");
            std::mem::replace(&mut state.initial, Arc::new(snapshot))
        };
        drop(previous);
        child
    }
}
struct ReturnOwner<'a>(&'a MetaInfoScope);
impl Drop for ReturnOwner<'_> {
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
#[pin_project]
struct ChildScope<F> {
    parent: TaskKey,
    snapshot: Option<metainfo::MetaInfo>,
    #[pin]
    future: F,
    // Dropped after the application future and its cancellation guards.
    cleanup: SnapshotLease,
}
struct SnapshotLease {
    owner: MetaInfoScope,
    key: Option<TaskKey>,
}
impl Drop for SnapshotLease {
    fn drop(&mut self) {
        if let Some(key) = self.key {
            let snapshot = self.owner.0.lock().tasks.remove(&key);
            drop(snapshot);
        }
    }
}
impl<F: Future> Future for ChildScope<F> {
    type Output = F::Output;
    fn poll(self: std::pin::Pin<&mut Self>, task: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        if let Some(snapshot) = this.snapshot.take() {
            let key = TaskKey::current();
            if key != *this.parent {
                this.cleanup.key = Some(key);
                let previous = this
                    .cleanup
                    .owner
                    .0
                    .lock()
                    .tasks
                    .entry(key)
                    .replace(snapshot);
                drop(previous);
            }
        }
        this.cleanup.owner.sync_scope(|| this.future.poll(task))
    }
}

#[cfg(test)]
mod tests {
    use metainfo::{Forward, METAINFO};

    use super::*;

    #[tokio::test]
    async fn task_scope_awaited_by_its_creator_preserves_the_parent_snapshot() {
        let owner = MetaInfoScope::default();
        owner.update(|info| info.set_persistent("key", "captured"));
        let child = owner.task_scope(async {});
        owner.update(|info| info.set_persistent("key", "latest"));
        child.await;
        assert_eq!(
            owner.with(|info| info.get_persistent("key")).as_deref(),
            Some("latest")
        );
    }

    #[tokio::test]
    async fn admission_handoff_preserves_input_and_reclaims_task_snapshots() {
        let owner = MetaInfoScope::default();
        owner.update(|info| info.set_persistent("key", "admitted"));
        let handler = owner.clone();
        let task = owner.handoff(async move {
            assert_eq!(
                handler.with(|info| info.get_persistent("key")).as_deref(),
                Some("admitted")
            );
            handler.update(|info| info.set_persistent("key", "handler-only"));
        });
        assert!(owner.0.lock().tasks.is_empty(), "admission entry released");
        tokio::spawn(task).await.unwrap();
        assert!(owner.0.lock().tasks.is_empty(), "handler entry released");
        assert_eq!(
            owner.with(|info| info.get_persistent("key")).as_deref(),
            Some("admitted"),
            "later driver tasks start from admitted input, not handler updates"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn managed_tasks_keep_independent_snapshots_and_return_results_explicitly() {
        let owner = MetaInfoScope::default();
        owner.update(|info| info.set_persistent("key", "parent"));
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut tasks = Vec::new();
        for value in ["first", "second"] {
            let child = owner.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(owner.task_scope(async move {
                assert_eq!(
                    child.with(|info| info.get_persistent("key")).as_deref(),
                    Some("parent")
                );
                child.update(|info| info.set_persistent("key", value));
                barrier.wait().await;
                tokio::task::yield_now().await;
                assert_eq!(
                    child.with(|info| info.get_persistent("key")).as_deref(),
                    Some(value)
                );
                value
            })));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(
            owner.with(|info| info.get_persistent("key")).as_deref(),
            Some("parent")
        );
        assert_eq!(
            owner.0.lock().tasks.len(),
            1,
            "managed task snapshots are reclaimed"
        );
    }

    #[tokio::test]
    async fn a_surviving_task_keeps_its_snapshot_when_the_primary_task_exits() {
        let owner = MetaInfoScope::default();
        let (ready, started) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let first = tokio::spawn(owner.handoff(async move {
            ready.send(()).unwrap();
            released.await.unwrap();
        }));
        started.await.unwrap();
        let (ready, started) = tokio::sync::oneshot::channel();
        let (resume, resumed) = tokio::sync::oneshot::channel();
        let other = owner.clone();
        let second = tokio::spawn(async move {
            other
                .scope(async {
                    other.update(|info| info.set_persistent("survivor", "retained"));
                    ready.send(()).unwrap();
                    resumed.await.unwrap();
                    assert_eq!(
                        other
                            .with(|info| info.get_persistent("survivor"))
                            .as_deref(),
                        Some("retained")
                    );
                })
                .await;
        });
        started.await.unwrap();
        release.send(()).unwrap();
        first.await.unwrap();
        resume.send(()).unwrap();
        second.await.unwrap();
    }

    #[test]
    fn read_callbacks_can_reenter_without_holding_the_state_mutex() {
        let owner = MetaInfoScope::default();
        owner.with(|_| owner.with(|_| ()));
    }

    #[test]
    fn cross_scope_access_returns_the_current_value_before_waiting() {
        let first = MetaInfoScope::default();
        let second = MetaInfoScope::default();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let (done, completions) = std::sync::mpsc::channel();
        let threads: Vec<_> = [(first.clone(), second.clone()), (second, first)]
            .into_iter()
            .map(|(own, other)| {
                let barrier = barrier.clone();
                let done = done.clone();
                std::thread::spawn(move || {
                    own.sync_scope(|| {
                        barrier.wait();
                        other.with(|_| ());
                    });
                    done.send(()).unwrap();
                })
            })
            .collect();
        for _ in 0..2 {
            completions
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap();
        }
        for thread in threads {
            thread.join().unwrap();
        }
    }

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
