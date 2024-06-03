#![allow(dead_code)]
//! These codes are originally copied from `hyper/client/pool.rs` with a lot of modifications.

mod make_transport;
mod started;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::Debug,
    future::Future,
    hash::Hash,
    ops::{Deref, DerefMut},
    sync::{Arc, Weak},
};

use futures::future::{self, Either};
use linked_hash_map::LinkedHashMap;
pub use make_transport::PooledMakeTransport;
use motore::service::UnaryService;
use parking_lot::Mutex;
use pilota::thrift::TransportException;
use pin_project::pin_project;
use started::Started as _;
use tokio::{
    sync::oneshot,
    time::{interval, Duration, Instant, Interval},
};
use volo::Unwrap;

pub trait Key: Eq + Hash + Clone + Debug + Unpin + Send + Sync + 'static {}

impl<T> Key for T where T: Eq + Hash + Clone + Debug + Unpin + Send + Sync + 'static {}

/// A marker to identify what version a pooled connection is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ver {
    PingPong,
    Multiplex,
}

pub trait Poolable: Send + Sync + Sized {
    // check if the connection is opened
    fn reusable(&self) -> impl Future<Output = bool> + Send;

    /// Reserve this connection.
    ///
    /// Allows for HTTP/2, pipeline etc to return a shared reservation.
    fn reserve(self) -> Reservation<Self> {
        Reservation::Unique(self)
    }

    // put back into pool before check shareable
    fn can_share(&self) -> bool {
        false
    }
}

/// When checking out a pooled connection, it might be that the connection
/// only supports a single reservation, or it might be usable for many.
///
/// Specifically, HTTP/1 requires a unique reservation, but HTTP/2 can be
/// used for multiple requests.
// FIXME: allow() required due to `impl Trait` leaking types to this lint
#[allow(missing_debug_implementations)]
pub enum Reservation<T> {
    /// This connection could be used multiple times, the first one will be
    /// reinserted into the `idle` pool, and the second will be given to
    /// the `waiter`.
    Shared(T, T),
    /// This connection requires unique access. It will be returned after
    /// use is complete.
    Unique(T),
}

/// Connection Pool for reuse connections
pub struct Pool<K: Key, T: Poolable> {
    // share between threads
    inner: Arc<Inner<K, T>>,
}

impl<K: Key, T: Poolable> Clone for Pool<K, T> {
    fn clone(&self) -> Self {
        Pool {
            inner: self.inner.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    max_idle_per_key: usize,
    timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            max_idle_per_key: 10240,
            timeout: Duration::from_secs(15),
        }
    }
}

impl Config {
    pub fn new(max_idle_per_key: usize, timeout: Duration) -> Self {
        Config {
            max_idle_per_key,
            timeout,
        }
    }

    pub fn max_idle_per_key(mut self, max_idle_per_key: usize) -> Self {
        self.max_idle_per_key = max_idle_per_key;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

// This is because `Weak::new()` *allocates* space for `T`, even if it
// doesn't need it!
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

struct Expiration(Option<Duration>);

impl Expiration {
    fn new(dur: Option<Duration>) -> Expiration {
        Expiration(dur)
    }

    fn expires(&self, instant: Instant) -> bool {
        match self.0 {
            // Avoid `Instant::elapsed` to avoid issues like rust-lang/rust#86470.
            Some(timeout) => Instant::now().saturating_duration_since(instant) > timeout,
            None => false,
        }
    }
}

/// Pop off this list, looking for a usable connection that hasn't expired.
struct IdlePopper<'a, K: Key, T> {
    key: &'a K,
    list: &'a mut VecDeque<Idle<T>>,
}

impl<'a, K: Key, T: Poolable + 'a> IdlePopper<'a, K, T> {
    async fn pop(self, expiration: &Expiration) -> Option<Idle<T>> {
        while let Some(entry) = self.list.pop_front() {
            // If the connection has been closed, or is older than our idle
            // timeout, simply drop it and keep looking...
            if !entry.inner.reusable().await {
                tracing::info!("[VOLO] removing closed connection for {:?}", self.key);
                continue;
            }
            // TODO: Actually, since the `idle` list is pushed to the end always,
            // that would imply that if *this* entry is expired, then anything
            // "earlier" in the list would *have* to be expired also... Right?
            //
            // In that case, we could just break out of the loop and drop the
            // whole list...
            if expiration.expires(entry.idle_at) {
                tracing::trace!("[VOLO] removing expired connection for {:?}", self.key);
                continue;
            }

            let value = match entry.inner.reserve() {
                Reservation::Shared(to_reinsert, to_return) => {
                    self.list.push_back(Idle {
                        idle_at: Instant::now(),
                        inner: to_reinsert,
                    });
                    to_return
                }
                Reservation::Unique(unique) => unique,
            };

            return Some(Idle {
                idle_at: entry.idle_at,
                inner: value,
            });
        }

        None
    }
}

impl<K: Key, T: Poolable + Send + 'static> Pool<K, T> {
    #[allow(dead_code)]
    pub fn new(cfg: Option<Config>) -> Self {
        let cfg = cfg.unwrap_or_default();
        let (tx, rx) = oneshot::channel();
        let inner = Arc::new(Inner {
            connecting: Mutex::new(HashSet::new()),
            idles: tokio::sync::Mutex::new(HashMap::new()),
            waiters: Mutex::new(HashMap::new()),
            timeout: cfg.timeout,
            max_idle_per_key: cfg.max_idle_per_key,
            _pool_drop_rx: rx,
        });

        let idle_task = IdleTask {
            interval: interval(cfg.timeout),
            inner: Arc::downgrade(&inner),
            pool_drop_tx: tx,
        };
        tokio::spawn(idle_task.clear_expired());
        Pool { inner }
    }

    /// Ensure that there is only ever 1 connecting task for Multiplex
    /// connections. This does nothing for PingPong.
    pub fn connecting(&self, key: &K, ver: Ver) -> Option<Connecting<K, T>> {
        if ver == Ver::Multiplex {
            let mut connecting = self.inner.connecting.lock();
            return if connecting.insert(key.clone()) {
                let connecting = Connecting {
                    key: key.clone(),
                    pool: WeakOpt::downgrade(&self.inner),
                };
                tracing::trace!("Multiplex connecting for {:?}", key);
                Some(connecting)
            } else {
                tracing::trace!("Multiplex connecting already in progress for {:?}", key);
                None
            };
        }

        // else
        Some(Connecting {
            key: key.clone(),
            // in PingPong's case, there is never a lock, so we don't
            // need to do anything in Drop.
            pool: WeakOpt::none(),
        })
    }

    pub async fn get<MT>(
        &self,
        key: K,
        ver: Ver,
        mt: MT,
    ) -> Result<Pooled<K, T>, crate::ClientError>
    where
        T: Poolable + Send + 'static,
        MT: UnaryService<K, Response = T> + Send + 'static + Sync,
        MT::Error: Into<crate::ClientError> + Send,
    {
        let (rx, _waiter_token) = {
            let mut idles = self.inner.idles.lock().await;
            // 1. check the idle and opened connections
            let expiration = Expiration::new(Some(self.inner.timeout));
            let entry = if let Some(list) = idles.get_mut(&key) {
                tracing::trace!("[VOLO] take? {:?}: expiration = {:?}", key, expiration.0);
                {
                    let popper = IdlePopper { key: &key, list };
                    popper.pop(&expiration).await
                }
            } else {
                None
            };
            drop(idles);

            if let Some(t) = entry {
                return Ok(self.reuse(&key, t.inner));
            }
            // 2. no valid idle then add caller into waiters and make connection
            let mut waiters = self.inner.waiters.lock();
            let waiter_list = if let Some(waiter) = waiters.get_mut(&key) {
                waiter
            } else {
                waiters.entry(key.clone()).or_default()
            };
            let (tx, rx) = oneshot::channel();
            (rx, waiter_list.insert(tx))
            // drop lock guard before await
        };

        // 3. select waiter and mc return future
        let connector = {
            let key = key.clone();
            let this = self.clone();
            move || {
                Box::pin(async move {
                    match this.connecting(&key, ver) {
                        Some(connecting) => match mt.call(key).await {
                            Ok(t) => {
                                tracing::debug!(
                                    "[VOLO] make_transport finished for {:?}",
                                    &connecting.key
                                );
                                Ok(this.pooled(connecting, t).await)
                            }
                            Err(e) => Err(e),
                        },
                        None => future::pending().await,
                    }
                })
            }
        };

        // waiter or make transport finished
        match future::select(rx, started::lazy(connector)).await {
            Either::Left((Ok(v), fut)) => {
                // check the make transport future has started
                if fut.started() {
                    // complete the make transport and put into pool
                    tokio::spawn(fut);
                }
                // get connection from pool
                Ok(self.reuse(&key, v))
            }
            Either::Right((Ok(v), _)) => {
                tracing::debug!("[VOLO] get connection from pool for {:?}", key);
                Ok(v)
            }
            // means connection pool is dropped
            Either::Left((Err(e), _)) => {
                tracing::error!("[VOLO] wait a idle connection error: {:?}", e);
                Err(TransportException::from(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("wait a idle connection error: {:?}", e),
                ))
                .into())
            }
            // maybe there is no more connection put back into pool and waiter will block forever,
            // so just return error
            Either::Right((Err(e), _)) => {
                let e = e.into();
                tracing::error!("[VOLO] create connection error: {:?}, key: {:?}", e, key);
                Err(e)
            }
        }
    }

    async fn pooled(&self, mut connecting: Connecting<K, T>, value: T) -> Pooled<K, T> {
        let (value, pool_ref) = {
            match value.reserve() {
                Reservation::Shared(to_insert, to_return) => {
                    self.inner.put(connecting.key.clone(), to_insert).await;
                    self.inner.connected(&connecting.key);
                    connecting.pool = WeakOpt::none();
                    // Shared reservations don't need a reference to the pool,
                    // since the pool always keeps a copy.
                    (to_return, None)
                }
                Reservation::Unique(value) => {
                    // Unique reservations must take a reference to the pool
                    // since they hope to reinsert once the reservation is
                    // completed
                    (value, Some(Arc::downgrade(&self.inner)))
                }
            }
        };
        Pooled::new(connecting.key.clone(), value, WeakOpt(pool_ref))
    }

    fn reuse(&self, key: &K, value: T) -> Pooled<K, T> {
        tracing::debug!("[VOLO] reuse idle connection for {:?}", key);
        // TODO: unhack this
        // In Pool::pooled(), which is used for inserting brand new connections,
        // there's some code that adjusts the pool reference taken depending
        // on if the Reservation can be shared or is unique. By the time
        // reuse() is called, the reservation has already been made, and
        // we just have the final value, without knowledge of if this is
        // unique or shared.
        let mut pool_ref = None;
        if !value.can_share() {
            pool_ref = Some(Arc::downgrade(&self.inner));
        }
        Pooled::new(key.clone(), value, WeakOpt(pool_ref))
    }
}

pub struct Connecting<K: Key, T: Poolable> {
    key: K,
    pool: WeakOpt<Inner<K, T>>,
}

impl<K: Key, T> Connecting<K, T>
where
    T: Poolable + Send + 'static,
{
    pub fn multiplex(self, pool: &Pool<K, T>) -> Option<Self> {
        pool.connecting(&self.key, Ver::Multiplex)
    }
}

impl<K: Key, T: Poolable> Drop for Connecting<K, T> {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade() {
            pool.connected(&self.key);
        }
    }
}

struct Idle<T> {
    inner: T,
    idle_at: Instant,
}

#[pin_project]
pub struct Pooled<K: Key, T: Poolable> {
    key: Option<K>,
    #[pin]
    t: Option<T>,
    // shared transport no need pool ref
    pool: WeakOpt<Inner<K, T>>,
}

impl<K: Key, T: Poolable> Pooled<K, T> {
    fn new(key: K, t: T, pool: WeakOpt<Inner<K, T>>) -> Self {
        Pooled {
            key: Some(key),
            t: Some(t),
            pool,
        }
    }

    pub(crate) async fn reuse(mut self) {
        let inner = self.t.take().volo_unwrap();
        if !inner.reusable().await {
            // If we *already* know the connection is done here,
            // it shouldn't be re-inserted back into the pool.
            return;
        }
        // let pool = self.pool.clone();
        let key = self.key.take().volo_unwrap();
        if let WeakOpt(Some(pool)) = self.pool {
            if let Some(pool) = pool.upgrade() {
                pool.put(key, inner).await;
            }
        }
    }
}

impl<K: Key, T: Poolable> AsRef<T> for Pooled<K, T> {
    fn as_ref(&self) -> &T {
        self.t.as_ref().expect("not dropped")
    }
}

impl<K: Key, T: Poolable> AsMut<T> for Pooled<K, T> {
    fn as_mut(&mut self) -> &mut T {
        self.t.as_mut().expect("not dropped")
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

struct WaiterList<T> {
    inner: LinkedHashMap<usize, oneshot::Sender<T>>,
    counter: usize,
}

impl<T> Default for WaiterList<T> {
    fn default() -> Self {
        Self {
            inner: Default::default(),
            counter: 0,
        }
    }
}

impl<T> WaiterList<T> {
    pub fn pop(&mut self) -> Option<oneshot::Sender<T>> {
        self.inner.pop_front().map(|(_, v)| v)
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn insert(&mut self, sender: oneshot::Sender<T>) -> usize {
        let index = self.counter;
        self.counter = self.counter.wrapping_add(1);
        self.inner.insert(index, sender);
        index
    }

    pub fn remove(&mut self, index: usize) -> Option<oneshot::Sender<T>> {
        self.inner.remove(&index)
    }
}

struct Inner<K: Key, T: Poolable> {
    // A flag that a connection is being established, and the connection
    // should be shared. This prevents making multiple Multiplex connections
    // to the same host.
    connecting: Mutex<HashSet<K>>,
    // idle queue
    idles: tokio::sync::Mutex<HashMap<K, VecDeque<Idle<T>>>>,
    // waiters wait for idle transport
    waiters: Mutex<HashMap<K, WaiterList<T>>>,
    // idle timeout and check interval
    timeout: Duration,
    // idle count per key
    max_idle_per_key: usize,
    // when rx dropped, then tx poll_closed will return Poll::Ready(())
    // then idle task exist
    _pool_drop_rx: oneshot::Receiver<()>,
}

impl<K: Key, T: Poolable> Inner<K, T> {
    // clear expired idle
    async fn clear_expired(&self) {
        let timeout = self.timeout;
        let now = Instant::now();
        let mut keys = Vec::new();
        let mut idles = self.idles.lock().await;

        for (key, values) in idles.iter_mut() {
            // copied from .retain()
            let len = values.len();
            let mut idx = 0;
            let mut cur = 0;

            // Stage 1: All values are retained.
            while cur < len {
                if !f(&values[cur], key, now, timeout).await {
                    cur += 1;
                    break;
                }
                cur += 1;
                idx += 1;
            }
            // Stage 2: Swap retained value into current idx.
            while cur < len {
                if !f(&values[cur], key, now, timeout).await {
                    cur += 1;
                    continue;
                }

                values.swap(idx, cur);
                cur += 1;
                idx += 1;
            }
            // Stage 3: Truncate all values after idx.
            if cur != idx {
                values.truncate(idx);
            }

            if values.is_empty() {
                keys.push(key.clone());
            }
        }

        for key in keys {
            idles.remove(&key);
        }
    }
}

async fn f(idle: &Idle<impl Poolable>, key: &impl Key, now: Instant, timeout: Duration) -> bool {
    // TODO: check has_idle && remove the (idle, waiters) key
    if !idle.inner.reusable().await {
        tracing::trace!("[VOLO] removing closed connection for {:?}", key);
        return false;
    }
    if now - idle.idle_at > timeout {
        tracing::trace!("[VOLO] removing expired connection for {:?}", key);
        return false;
    }
    true
}

impl<K: Key, T: Poolable> Inner<K, T> {
    async fn put(&self, key: K, t: T) {
        // check the wait queue
        let mut value = Some(t);
        {
            let mut waiters = self.waiters.lock();
            if let Some(waiter_list) = waiters.get_mut(&key) {
                // find a waiter and send
                while let Some(waiter) = waiter_list.pop() {
                    // check if waiter is dropped
                    if !waiter.is_closed() {
                        let t = value.take().volo_unwrap();
                        let t = match t.reserve() {
                            Reservation::Shared(to_keep, to_send) => {
                                value = Some(to_keep);
                                to_send
                            }
                            Reservation::Unique(unique) => unique,
                        };
                        match waiter.send(t) {
                            Ok(()) => {
                                tracing::trace!("[VOLO] [pool put]: found waiter for {:?}", key);
                                if value.is_none() {
                                    // Unique break
                                    break;
                                }
                            }
                            Err(t) => {
                                value = Some(t);
                            }
                        }
                    }
                }
                // if waiters is empty then remove from waiters
                if waiter_list.is_empty() {
                    waiters.remove(&key);
                }
            }
            drop(waiters);
        }

        // check if send to some waiter
        let mut idles = self.idles.lock().await;
        if let Some(t) = value {
            if t.can_share() && idles.contains_key(&key) {
                tracing::trace!(
                    "[VOLO] put; existing idle Shareable connection for {:?}",
                    key
                );
                return;
            }
            // means doesn't send success
            // then put back to idle list
            let idle = idles.entry(key).or_default();
            if idle.len() < self.max_idle_per_key {
                idle.push_back(Idle {
                    inner: t,
                    idle_at: Instant::now(),
                });
            }
        }
    }

    /// A `Connecting` task is complete. Not necessarily successfully,
    /// but the lock is going away, so clean up.
    fn connected(&self, key: &K) {
        let existed = self.connecting.lock().remove(key);
        debug_assert!(existed, "Connecting dropped, key not in pool.connecting");
        // cancel any waiters. if there are any, it's because
        // this Connecting task didn't complete successfully.
        // those waiters would never receive a connection.
        self.waiters.lock().remove(key);
    }
}

// Idle refresh task
struct IdleTask<K: Key, T: Poolable> {
    // refresh interval
    interval: Interval,
    // pool
    inner: Weak<Inner<K, T>>,
    // drop tx and rx recv error
    pool_drop_tx: oneshot::Sender<()>,
}

impl<K: Key, T: Poolable> IdleTask<K, T> {
    async fn clear_expired(mut self) {
        loop {
            tokio::select! {
                _ = self.interval.tick() => {
                    if let Some(inner) = self.inner.upgrade() {
                        tracing::trace!("[VOLO] idle interval checking for expired");
                        inner.clear_expired().await;
                    } else {
                        return;
                    }
                }
                _ = self.pool_drop_tx.closed() => {
                    tracing::trace!("[VOLO] pool closed, canceling idle interval");
                    return;
                }

            }
        }
    }
}
