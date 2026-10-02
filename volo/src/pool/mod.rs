//! Generic transport pool shared by the protocol crates.
//!
//! A [`Pool`] keeps transports (connections, or handles multiplexing a connection) per [`Key`],
//! usually the callee [`Address`][crate::net::Address] chosen by service discovery and load
//! balancing. Transports come in two flavours, chosen per [`Pool::get`] with a [`Mode`]:
//!
//! - [`Mode::Unique`]: one request at a time. The caller gets exclusive use of the transport and
//!   hands it back with [`Pooled::reuse`] once done; dropping it without `reuse` discards it.
//! - [`Mode::Shared`]: multiplexed. The pool keeps one transport per key and hands out clones (see
//!   [`Poolable::reserve`]); concurrent callers hitting an empty key wait for the single in-flight
//!   connect instead of each dialing.
//!
//! Transports idle for longer than [`Config::idle_timeout`] are evicted by a background task that
//! is started on first use and stops when the pool is dropped.
//!
//! Protocol crates plug in with two impls: [`Poolable`] on the transport, and a
//! [`UnaryService<K>`] that makes a new one; the pool is agnostic to the error type and reports
//! it back through [`Error::Connect`].

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::Debug,
    future::Future,
    hash::Hash,
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, PoisonError, Weak},
    task::{Context, Poll, ready},
    time::Duration,
};

use futures::future::{self, Either};
use linked_hash_map::LinkedHashMap;
use motore::service::UnaryService;
use pin_project::pin_project;
use tokio::{
    sync::oneshot,
    task::JoinHandle,
    time::{Instant, Interval, interval},
};

/// Identifies the peer a transport belongs to; usually [`crate::net::Address`].
pub trait Key: Eq + Hash + Clone + Debug + Unpin + Send + 'static {}

impl<T> Key for T where T: Eq + Hash + Clone + Debug + Unpin + Send + 'static {}

/// How transports for a key are handed out, see the [module docs][self].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Exclusive use; returned with [`Pooled::reuse`].
    Unique,
    /// Multiplexed; one transport per key, handed out as clones.
    Shared,
}

/// A transport that can live in a [`Pool`].
pub trait Poolable: Sized {
    /// Whether the transport can still be used. Called before handing an idle transport out
    /// and before putting one back.
    fn reusable(&self) -> impl Future<Output = bool> + Send;

    /// Splits the transport into the copy kept by the pool and the one handed to the caller.
    ///
    /// Shared transports return [`Reservation::Shared`]; the default is exclusive use.
    fn reserve(self) -> Reservation<Self> {
        Reservation::Unique(self)
    }

    /// Whether [`Self::reserve`] returns [`Reservation::Shared`].
    fn can_share(&self) -> bool {
        false
    }

    /// Fast, synchronous checkout for shared transports: a clone if the transport is known to be
    /// usable, `None` to fall back to the async [`Self::reusable`] check.
    fn try_checkout(&self) -> Option<Self> {
        None
    }
}

/// Result of [`Poolable::reserve`].
#[allow(missing_debug_implementations)]
pub enum Reservation<T> {
    /// The first copy stays in the pool, the second goes to the caller.
    Shared(T, T),
    /// The transport is handed to the caller exclusively.
    Unique(T),
}

/// Error returned by [`Pool::get`].
#[derive(Debug, thiserror::Error)]
pub enum Error<E> {
    /// Making a new transport failed.
    #[error("failed to make a new transport: {0}")]
    Connect(E),
    /// Waiting for a transport was given up: the pool was dropped, or the in-flight connect
    /// that this caller was waiting on failed.
    #[error("waiting for a pooled transport was canceled")]
    Canceled,
}

/// Pool configuration.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    max_idle_per_key: usize,
    idle_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            max_idle_per_key: 10240,
            idle_timeout: Duration::from_secs(15),
        }
    }
}

impl Config {
    #[must_use]
    pub fn new(max_idle_per_key: usize, idle_timeout: Duration) -> Self {
        Config {
            max_idle_per_key,
            idle_timeout,
        }
    }

    /// Maximum idle transports kept per key. Shared transports are always capped at one.
    #[must_use]
    pub fn max_idle_per_key(mut self, max_idle_per_key: usize) -> Self {
        self.max_idle_per_key = max_idle_per_key;
        self
    }

    /// How long a transport may sit idle before being evicted; also the eviction check period.
    #[must_use]
    pub fn idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = idle_timeout;
        self
    }
}

/// A transport pool, cheap to clone; all clones share the same transports.
pub struct Pool<K: Key, T: Poolable> {
    inner: Arc<Mutex<Inner<K, T>>>,
}

impl<K: Key, T: Poolable> Clone for Pool<K, T> {
    fn clone(&self) -> Self {
        Pool {
            inner: self.inner.clone(),
        }
    }
}

impl<K: Key, T: Poolable> Debug for Pool<K, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool").finish_non_exhaustive()
    }
}

impl<K: Key, T: Poolable + Send + 'static> Pool<K, T> {
    /// Creates an empty pool. Safe to call outside a runtime; the idle-eviction task starts on
    /// the first [`Self::get`].
    #[must_use]
    pub fn new(cfg: Config) -> Self {
        let (tx, rx) = oneshot::channel();
        Pool {
            inner: Arc::new(Mutex::new(Inner {
                connecting: HashSet::new(),
                idle: HashMap::new(),
                waiters: HashMap::new(),
                idle_timeout: cfg.idle_timeout,
                max_idle_per_key: cfg.max_idle_per_key,
                idle_task_tx: Some(tx),
                _pool_drop_rx: rx,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner<K, T>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn ensure_idle_task(&self, inner: &mut Inner<K, T>) {
        if let Some(tx) = inner.idle_task_tx.take() {
            tokio::spawn(IdleTask {
                interval: interval(inner.idle_timeout),
                inner: Arc::downgrade(&self.inner),
                pool_drop_tx: tx,
            });
        }
    }

    /// Takes the "connecting lock" for `key`.
    ///
    /// Shared transports allow a single in-flight connect per key, so this returns `None` if
    /// one is already running; unique transports never contend.
    fn connecting(&self, key: &K, mode: Mode) -> Option<Connecting<K, T>> {
        match mode {
            Mode::Shared => {
                let mut inner = self.lock();
                if inner.connecting.insert(key.clone()) {
                    tracing::trace!("[VOLO] shared connecting for {key:?}");
                    Some(Connecting {
                        key: key.clone(),
                        pool: WeakOpt::downgrade(&self.inner),
                    })
                } else {
                    tracing::trace!("[VOLO] shared connecting already in progress for {key:?}");
                    None
                }
            }
            // Never locked, so there is nothing to release on drop.
            Mode::Unique => Some(Connecting {
                key: key.clone(),
                pool: WeakOpt::none(),
            }),
        }
    }

    /// Returns a transport for `key`: an idle one if there is a usable one, otherwise whichever
    /// comes first of a transport handed back by another caller and a freshly made one via `mt`.
    ///
    /// # Errors
    ///
    /// [`Error::Connect`] carries `mt`'s error when making a new transport fails.
    /// [`Error::Canceled`] means this caller was waiting on someone else's connect for a shared
    /// key and that connect failed, or the pool was dropped meanwhile.
    pub async fn get<MT>(
        &self,
        key: K,
        mode: Mode,
        mt: MT,
    ) -> Result<Pooled<K, T>, Error<MT::Error>>
    where
        MT: UnaryService<K, Response = T> + Send + Sync + 'static,
        MT::Error: Send + 'static,
    {
        let checkout = {
            let entry = 'outer: loop {
                let entry = 'inner: {
                    let mut inner = self.lock();
                    self.ensure_idle_task(&mut inner);
                    let idle_timeout = inner.idle_timeout;

                    let Some(list) = inner.idle.get_mut(&key) else {
                        break 'outer None;
                    };

                    // Fast path: shared transports can be checked out synchronously under the
                    // lock, which also avoids a race where the list looks empty right after a
                    // pop and a second transport gets made needlessly.
                    while list.front().is_some_and(|e| e.inner.can_share()) {
                        if list[0].expired(idle_timeout) {
                            list.pop_front();
                            continue;
                        }
                        if let Some(t) = list[0].inner.try_checkout() {
                            list[0].idle_at = Instant::now();
                            return Ok(self.reuse(&key, t));
                        }
                        // Unknown or broken: fall through to the async `reusable` check.
                        break;
                    }

                    while let Some(entry) = list.pop_front() {
                        if entry.expired(idle_timeout) {
                            tracing::trace!("[VOLO] dropping expired idle transport for {key:?}");
                            continue;
                        }
                        break 'inner entry;
                    }
                    break 'outer None;
                };
                // Closed underneath us: drop it and keep looking.
                if !entry.inner.reusable().await {
                    continue;
                }
                break 'outer Some(entry);
            };

            let mut inner = self.lock();
            if let Some(entry) = entry {
                let t = match entry.inner.reserve() {
                    Reservation::Shared(to_keep, to_return) => {
                        if let Some(list) = inner.idle.get_mut(&key) {
                            list.push_back(Idle::new(to_keep));
                        }
                        to_return
                    }
                    Reservation::Unique(t) => t,
                };
                return Ok(self.reuse(&key, t));
            }

            // Nothing idle: queue as a waiter, then race that against making a new transport.
            let (tx, rx) = oneshot::channel();
            let token = inner.waiters.entry(key.clone()).or_default().insert(tx);
            Checkout {
                key: key.clone(),
                pool: self.clone(),
                waiter: rx,
                token,
                clean: true,
            }
            // lock released here, before any await
        };

        let Some(connecting) = self.connecting(&key, mode) else {
            // A shared connect is already in flight for this key; it will serve us as a waiter.
            let t = checkout.await.map_err(|_| Error::Canceled)?;
            return Ok(self.reuse(&key, t));
        };

        // The connect runs as its own task so that this caller giving up (an rpc timeout, say)
        // does not tear down the transport every other caller to this key is waiting for.
        let connect = ConnectTask {
            handle: {
                let pool = self.clone();
                let key = key.clone();
                tokio::spawn(async move {
                    let t = mt.call(key).await?;
                    tracing::debug!("[VOLO] made transport for {:?}", connecting.key);
                    Ok(pool.pooled(connecting, t))
                })
            },
            // A unique transport nobody is going to use is not worth finishing.
            abort_on_drop: mode == Mode::Unique,
        };

        let connect = match future::select(checkout, connect).await {
            Either::Left((Ok(t), _connect)) => return Ok(self.reuse(&key, t)),
            // The waiter side is gone (pool dropped); our own connect can still deliver.
            Either::Left((Err(_), connect)) => connect,
            Either::Right((result, _checkout)) => return result,
        };
        connect.await
    }

    fn pooled(&self, mut connecting: Connecting<K, T>, t: T) -> Pooled<K, T> {
        let (t, pool) = match t.reserve() {
            Reservation::Shared(to_keep, to_return) => {
                let mut inner = self.lock();
                inner.put(connecting.key.clone(), to_keep);
                inner.connected(&connecting.key);
                connecting.pool = WeakOpt::none();
                // The pool keeps its own copy; the caller's clone needs no way back.
                (to_return, WeakOpt::none())
            }
            Reservation::Unique(t) => (t, WeakOpt::downgrade(&self.inner)),
        };
        Pooled::new(connecting.key.clone(), t, pool)
    }

    fn reuse(&self, key: &K, t: T) -> Pooled<K, T> {
        tracing::debug!("[VOLO] reusing idle transport for {key:?}");
        // Only unique transports need a way back into the pool.
        let pool = if t.can_share() {
            WeakOpt::none()
        } else {
            WeakOpt::downgrade(&self.inner)
        };
        Pooled::new(key.clone(), t, pool)
    }
}

/// A transport checked out of a [`Pool`]; derefs to the transport.
///
/// Unique transports go back to the pool with [`Self::reuse`]; dropping one without it discards
/// the transport. Shared transports need nothing, the pool keeps its own copy.
#[pin_project]
pub struct Pooled<K: Key, T: Poolable> {
    key: Option<K>,
    #[pin]
    t: Option<T>,
    pool: WeakOpt<Mutex<Inner<K, T>>>,
}

impl<K: Key, T: Poolable> Pooled<K, T> {
    fn new(key: K, t: T, pool: WeakOpt<Mutex<Inner<K, T>>>) -> Self {
        Pooled {
            key: Some(key),
            t: Some(t),
            pool,
        }
    }

    /// Hands a unique transport back to the pool, if it is still usable.
    pub async fn reuse(mut self) {
        let Some(t) = self.t.take() else { return };
        if !t.reusable().await {
            return;
        }
        let Some(key) = self.key.take() else { return };
        if let Some(pool) = self.pool.upgrade() {
            pool.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .put(key, t);
        }
    }
}

impl<K: Key, T: Poolable> AsRef<T> for Pooled<K, T> {
    fn as_ref(&self) -> &T {
        self.t.as_ref().expect("transport already handed back")
    }
}

impl<K: Key, T: Poolable> AsMut<T> for Pooled<K, T> {
    fn as_mut(&mut self) -> &mut T {
        self.t.as_mut().expect("transport already handed back")
    }
}

impl<K: Key, T: Poolable> Deref for Pooled<K, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.as_ref()
    }
}

impl<K: Key, T: Poolable> DerefMut for Pooled<K, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.as_mut()
    }
}

/// Holds the "connecting lock" for a shared key while a transport is being made.
struct Connecting<K: Key, T: Poolable> {
    key: K,
    pool: WeakOpt<Mutex<Inner<K, T>>>,
}

impl<K: Key, T: Poolable> Drop for Connecting<K, T> {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade() {
            // Never panic in drop.
            if let Ok(mut inner) = pool.lock() {
                inner.connected(&self.key);
            }
        }
    }
}

/// Waits for a transport handed back by another caller.
struct Checkout<K: Key, T: Poolable> {
    key: K,
    pool: Pool<K, T>,
    waiter: oneshot::Receiver<T>,
    token: usize,
    clean: bool,
}

impl<K: Key, T: Poolable> Future for Checkout<K, T> {
    type Output = Result<T, oneshot::error::RecvError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let out = ready!(Pin::new(&mut self.waiter).poll(cx));
        // The sender was popped from the waiter list to reach us; nothing to clean up.
        self.clean = false;
        Poll::Ready(out)
    }
}

impl<K: Key, T: Poolable> Drop for Checkout<K, T> {
    fn drop(&mut self) {
        if self.clean {
            tracing::trace!("[VOLO] checkout dropped for {:?}", self.key);
            if let Ok(mut inner) = self.pool.inner.lock() {
                if let Some(waiters) = inner.waiters.get_mut(&self.key) {
                    waiters.remove(self.token);
                }
            }
        }
    }
}

/// The spawned connect; optionally aborted when nobody wants its result any more.
struct ConnectTask<T> {
    handle: JoinHandle<T>,
    abort_on_drop: bool,
}

impl<K: Key, T: Poolable, E> Future for ConnectTask<Result<Pooled<K, T>, E>> {
    type Output = Result<Pooled<K, T>, Error<E>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let out = match ready!(Pin::new(&mut self.handle).poll(cx)) {
            Ok(Ok(pooled)) => Ok(pooled),
            Ok(Err(err)) => Err(Error::Connect(err)),
            Err(err) => {
                tracing::error!("[VOLO] connect task failed: {err}");
                Err(Error::Canceled)
            }
        };
        Poll::Ready(out)
    }
}

impl<T> Drop for ConnectTask<T> {
    fn drop(&mut self) {
        if self.abort_on_drop {
            self.handle.abort();
        }
    }
}

/// An optional weak reference, so shared transports can carry "no way back" cheaply.
struct WeakOpt<T>(Option<Weak<T>>);

impl<T> WeakOpt<T> {
    fn none() -> Self {
        WeakOpt(None)
    }

    fn downgrade(arc: &Arc<T>) -> Self {
        WeakOpt(Some(Arc::downgrade(arc)))
    }

    fn upgrade(&self) -> Option<Arc<T>> {
        self.0.as_ref().and_then(Weak::upgrade)
    }
}

struct Idle<T> {
    inner: T,
    idle_at: Instant,
}

impl<T> Idle<T> {
    fn new(inner: T) -> Self {
        Idle {
            inner,
            idle_at: Instant::now(),
        }
    }

    fn expired(&self, timeout: Duration) -> bool {
        self.idle_at.elapsed() > timeout
    }
}

/// FIFO of waiters with O(1) removal by token, for callers that give up.
struct WaiterList<T> {
    inner: LinkedHashMap<usize, oneshot::Sender<T>>,
    counter: usize,
}

impl<T> Default for WaiterList<T> {
    fn default() -> Self {
        Self {
            inner: LinkedHashMap::new(),
            counter: 0,
        }
    }
}

impl<T> WaiterList<T> {
    fn pop(&mut self) -> Option<oneshot::Sender<T>> {
        self.inner.pop_front().map(|(_, v)| v)
    }

    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn insert(&mut self, sender: oneshot::Sender<T>) -> usize {
        let token = self.counter;
        self.counter = self.counter.wrapping_add(1);
        self.inner.insert(token, sender);
        token
    }

    fn remove(&mut self, token: usize) -> Option<oneshot::Sender<T>> {
        self.inner.remove(&token)
    }
}

struct Inner<K: Key, T: Poolable> {
    /// Keys with a shared connect in flight; guards against dialing a peer twice.
    connecting: HashSet<K>,
    idle: HashMap<K, VecDeque<Idle<T>>>,
    waiters: HashMap<K, WaiterList<T>>,
    idle_timeout: Duration,
    max_idle_per_key: usize,
    /// Taken by the idle task when it starts; `None` afterwards.
    idle_task_tx: Option<oneshot::Sender<()>>,
    /// Dropped with the pool, which wakes the idle task so it can stop.
    _pool_drop_rx: oneshot::Receiver<()>,
}

impl<K: Key, T: Poolable> Inner<K, T> {
    fn clear_expired(&mut self) {
        let timeout = self.idle_timeout;
        self.idle.retain(|key, list| {
            list.retain(|entry| {
                let keep = !entry.expired(timeout);
                if !keep {
                    tracing::trace!("[VOLO] idle task evicting expired transport for {key:?}");
                }
                keep
            });
            !list.is_empty()
        });
    }

    /// Puts a transport back: to the first live waiter, else onto the idle list.
    fn put(&mut self, key: K, t: T) {
        let mut value = Some(t);
        if let Some(waiters) = self.waiters.get_mut(&key) {
            while let Some(waiter) = waiters.pop() {
                if waiter.is_closed() {
                    continue;
                }
                let t = value
                    .take()
                    .expect("value is present until a unique send succeeds");
                let to_send = match t.reserve() {
                    Reservation::Shared(to_keep, to_send) => {
                        value = Some(to_keep);
                        to_send
                    }
                    Reservation::Unique(t) => t,
                };
                match waiter.send(to_send) {
                    Ok(()) => {
                        tracing::trace!("[VOLO] put: served a waiter for {key:?}");
                        if value.is_none() {
                            break;
                        }
                    }
                    Err(t) => value = Some(t),
                }
            }
            if waiters.is_empty() {
                self.waiters.remove(&key);
            }
        }

        if let Some(t) = value {
            if t.can_share() && self.idle.contains_key(&key) {
                tracing::trace!("[VOLO] put: shared transport for {key:?} already idle");
                return;
            }
            let idle = self.idle.entry(key).or_default();
            if idle.len() < self.max_idle_per_key {
                idle.push_back(Idle::new(t));
            }
        }
    }

    /// A shared connect for `key` finished, successfully or not; release the lock. Any waiters
    /// left at this point were waiting on a connect that failed, so they are told to give up.
    fn connected(&mut self, key: &K) {
        let existed = self.connecting.remove(key);
        debug_assert!(existed, "Connecting dropped, key not in pool.connecting");
        self.waiters.remove(key);
    }
}

#[pin_project]
struct IdleTask<K: Key, T: Poolable> {
    #[pin]
    interval: Interval,
    inner: Weak<Mutex<Inner<K, T>>>,
    #[pin]
    pool_drop_tx: oneshot::Sender<()>,
}

impl<K: Key, T: Poolable> Future for IdleTask<K, T> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        loop {
            if this.pool_drop_tx.as_mut().poll_closed(cx).is_ready() {
                tracing::trace!("[VOLO] pool dropped, stopping idle task");
                return Poll::Ready(());
            }
            ready!(this.interval.as_mut().poll_tick(cx));
            let Some(inner) = this.inner.upgrade() else {
                return Poll::Ready(());
            };
            if let Ok(mut inner) = inner.lock() {
                inner.clear_expired();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use super::*;

    #[derive(Clone, Debug)]
    struct Conn {
        id: usize,
        shared: bool,
        closed: Arc<AtomicBool>,
    }

    impl Conn {
        fn is_open(&self) -> bool {
            !self.closed.load(Ordering::SeqCst)
        }
    }

    impl Poolable for Conn {
        async fn reusable(&self) -> bool {
            self.is_open()
        }

        fn reserve(self) -> Reservation<Self> {
            if self.shared {
                Reservation::Shared(self.clone(), self)
            } else {
                Reservation::Unique(self)
            }
        }

        fn can_share(&self) -> bool {
            self.shared
        }

        fn try_checkout(&self) -> Option<Self> {
            (self.shared && self.is_open()).then(|| self.clone())
        }
    }

    #[derive(Clone)]
    struct Maker {
        made: Arc<AtomicUsize>,
        delay: Duration,
        fail: bool,
        shared: bool,
    }

    impl Maker {
        fn new(shared: bool) -> Self {
            Maker {
                made: Arc::new(AtomicUsize::new(0)),
                delay: Duration::ZERO,
                fail: false,
                shared,
            }
        }

        fn made(&self) -> usize {
            self.made.load(Ordering::SeqCst)
        }
    }

    impl UnaryService<&'static str> for Maker {
        type Response = Conn;
        type Error = &'static str;

        async fn call(&self, _key: &'static str) -> Result<Conn, &'static str> {
            tokio::time::sleep(self.delay).await;
            if self.fail {
                return Err("boom");
            }
            Ok(Conn {
                id: self.made.fetch_add(1, Ordering::SeqCst),
                shared: self.shared,
                closed: Arc::default(),
            })
        }
    }

    fn pool() -> Pool<&'static str, Conn> {
        Pool::new(Config::default())
    }

    #[tokio::test]
    async fn unique_is_reused_after_being_handed_back() {
        let pool = pool();
        let maker = Maker::new(false);

        let a = pool.get("k", Mode::Unique, maker.clone()).await.unwrap();
        let id = a.id;
        a.reuse().await;
        let b = pool.get("k", Mode::Unique, maker.clone()).await.unwrap();

        assert_eq!(b.id, id);
        assert_eq!(maker.made(), 1);
    }

    #[tokio::test]
    async fn unique_held_transports_do_not_block_others() {
        let pool = pool();
        let maker = Maker::new(false);

        let a = pool.get("k", Mode::Unique, maker.clone()).await.unwrap();
        let b = pool.get("k", Mode::Unique, maker.clone()).await.unwrap();

        assert_ne!(a.id, b.id);
        assert_eq!(maker.made(), 2);
    }

    #[tokio::test]
    async fn unique_dropped_without_reuse_is_discarded() {
        let pool = pool();
        let maker = Maker::new(false);

        drop(pool.get("k", Mode::Unique, maker.clone()).await.unwrap());
        pool.get("k", Mode::Unique, maker.clone()).await.unwrap();

        assert_eq!(maker.made(), 2);
    }

    #[tokio::test]
    async fn shared_concurrent_callers_share_one_connect() {
        let pool = pool();
        let mut maker = Maker::new(true);
        maker.delay = Duration::from_millis(20);

        let conns = future::join_all((0..16).map(|_| {
            let pool = pool.clone();
            let maker = maker.clone();
            async move { pool.get("k", Mode::Shared, maker).await.unwrap().id }
        }))
        .await;

        assert!(conns.iter().all(|id| *id == conns[0]));
        assert_eq!(maker.made(), 1);
    }

    #[tokio::test]
    async fn shared_connect_survives_the_first_caller_giving_up() {
        let pool = pool();
        let mut maker = Maker::new(true);
        maker.delay = Duration::from_millis(50);

        let impatient = tokio::time::timeout(
            Duration::from_millis(5),
            pool.get("k", Mode::Shared, maker.clone()),
        );
        let patient = pool.get("k", Mode::Shared, maker.clone());
        let (impatient, patient) = future::join(impatient, patient).await;

        assert!(impatient.is_err(), "the first caller timed out");
        patient.expect("the waiter is served by the connect the first caller started");
        assert_eq!(maker.made(), 1);
    }

    #[tokio::test]
    async fn shared_closed_transport_is_replaced() {
        let pool = pool();
        let maker = Maker::new(true);

        let a = pool.get("k", Mode::Shared, maker.clone()).await.unwrap();
        a.closed.store(true, Ordering::SeqCst);
        let b = pool.get("k", Mode::Shared, maker.clone()).await.unwrap();

        assert_ne!(a.id, b.id);
        assert_eq!(maker.made(), 2);
    }

    #[tokio::test]
    async fn keys_are_independent() {
        let pool = pool();
        let maker = Maker::new(true);

        let a = pool.get("a", Mode::Shared, maker.clone()).await.unwrap();
        let b = pool.get("b", Mode::Shared, maker.clone()).await.unwrap();
        let a2 = pool.get("a", Mode::Shared, maker.clone()).await.unwrap();

        assert_ne!(a.id, b.id);
        assert_eq!(a.id, a2.id);
        assert_eq!(maker.made(), 2);
    }

    #[tokio::test]
    async fn connect_failure_is_reported() {
        let pool = pool();
        let mut maker = Maker::new(true);
        maker.fail = true;

        let err = pool.get("k", Mode::Shared, maker).await.err().unwrap();
        assert!(matches!(err, Error::Connect("boom")), "{err:?}");
    }

    #[tokio::test]
    async fn idle_transports_are_evicted() {
        let pool: Pool<&'static str, Conn> =
            Pool::new(Config::default().idle_timeout(Duration::from_millis(100)));
        let maker = Maker::new(true);

        let a = pool.get("k", Mode::Shared, maker.clone()).await.unwrap();
        // Keeping the transport in use refreshes its idle timestamp.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let a2 = pool.get("k", Mode::Shared, maker.clone()).await.unwrap();
        assert_eq!(a.id, a2.id);

        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(
            pool.lock().idle.is_empty(),
            "idle task evicted the transport"
        );
        let b = pool.get("k", Mode::Shared, maker.clone()).await.unwrap();
        assert_ne!(a.id, b.id);
    }

    #[tokio::test]
    async fn dropping_the_pool_stops_the_idle_task() {
        let pool = pool();
        let inner = Arc::downgrade(&pool.inner);
        pool.get("k", Mode::Shared, Maker::new(true)).await.unwrap();

        drop(pool);
        tokio::task::yield_now().await;

        assert!(
            inner.upgrade().is_none(),
            "the idle task holds no strong reference"
        );
    }
}
