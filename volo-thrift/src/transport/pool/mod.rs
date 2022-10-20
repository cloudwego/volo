#![allow(dead_code)]
//! These codes are originally copied from `hyper/client/pool.rs` with a lot of modifications.

mod make_transport;
mod started;

use std::{
    collections::HashMap,
    fmt::Debug,
    future::Future,
    hash::Hash,
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    task::{Context, Poll},
};

use futures::{
    future::{self, Either},
    ready,
};
use linked_hash_map::LinkedHashMap;
pub use make_transport::PooledMakeTransport;
use motore::{service::UnaryService, BoxError};
use pin_project::pin_project;
use started::Started as _;
use tokio::{
    sync::oneshot,
    time::{interval, Duration, Instant, Interval},
};
use volo::Unwrap;

pub trait Poolable: Sized {
    // check if the connection is opened
    fn reusable(&self) -> bool;

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
pub struct Pool<Key, T> {
    // share between threads
    inner: Arc<Mutex<Inner<Key, T>>>,
}

impl<Key, T> Clone for Pool<Key, T> {
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
struct IdlePopper<'a, Key, T> {
    key: &'a Key,
    list: &'a mut Vec<Idle<T>>,
}

impl<'a, Key: Debug, T: Poolable + 'a> IdlePopper<'a, Key, T> {
    fn pop(self, expiration: &Expiration) -> Option<Idle<T>> {
        while let Some(entry) = self.list.pop() {
            // If the connection has been closed, or is older than our idle
            // timeout, simply drop it and keep looking...
            if !entry.inner.reusable() {
                tracing::trace!("[VOLO] removing closed connection for {:?}", self.key);
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
                    self.list.push(Idle {
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

impl<Key, T> Pool<Key, T>
where
    Key: Clone + Eq + Hash + Debug + Send + 'static,
    T: Poolable + Send + 'static,
{
    #[allow(dead_code)]
    pub fn new(cfg: Option<Config>) -> Self {
        let cfg = cfg.unwrap_or_default();
        let (tx, rx) = oneshot::channel();
        let inner = Arc::new(Mutex::new(Inner {
            idle: HashMap::new(),
            waiters: HashMap::new(),
            timeout: cfg.timeout,
            max_idle_per_key: cfg.max_idle_per_key,
            _pool_drop_rx: rx,
        }));

        let idle_task = IdleTask {
            interval: interval(cfg.timeout),
            inner: Arc::downgrade(&inner),
            pool_drop_tx: tx,
        };
        tokio::spawn(idle_task);
        Pool { inner }
    }

    pub async fn get<MT>(&self, key: Key, mut mt: MT) -> Result<Pooled<Key, T>, BoxError>
    where
        MT: UnaryService<Key, Response = T> + Send + 'static,
        MT::Error: Into<BoxError>,
    {
        let (rx, _waiter_token) = {
            let mut inner = self.inner.lock().volo_unwrap();
            // 1. check the idle and opened connections
            let expiration = Expiration::new(Some(inner.timeout));
            let entry = inner.idle.get_mut(&key).and_then(|list| {
                tracing::trace!("[VOLO] take? {:?}: expiration = {:?}", key, expiration.0);
                {
                    let popper = IdlePopper { key: &key, list };
                    popper.pop(&expiration)
                }
            });

            if let Some(t) = entry {
                tracing::debug!("[VOLO] reuse connection from cache for {:?}", key);
                return Ok(self.reuse(&key, t.inner));
            }
            // 2. no valid idle then add caller into waiters and make connection
            let waiters = if let Some(waiter) = inner.waiters.get_mut(&key) {
                waiter
            } else {
                inner
                    .waiters
                    .entry(key.clone())
                    .or_insert_with(Default::default)
            };
            let (tx, rx) = oneshot::channel();
            (rx, waiters.insert(tx))
            // drop lock guard before await
        };

        // 3. select waiter and mc return future
        let lazy_fut = {
            let key = key.clone();
            move || Box::pin(async move { mt.call(key).await })
        };

        // waiter or make transport finished
        match future::select(rx, started::lazy(lazy_fut)).await {
            Either::Left((Ok(v), fut)) => {
                // check the make transport future has started
                if fut.started() {
                    let key = key.clone();
                    let this = self.clone();
                    // complete the make transport and put into pool
                    tokio::spawn(async move {
                        if let Ok(t) = fut.await {
                            // drop here and put back into pool
                            // spawn need 'static, so we move weak_pool from out scope
                            tracing::debug!("[VOLO] spawn make_transport finished for {:?}", key);
                            let _ = this.pooled(&key, t);
                        }
                    });
                }
                tracing::debug!("[VOLO] reuse connection from waiter for {:?}", key);
                // get connection from pool
                Ok(self.reuse(&key, v))
            }
            Either::Right((Ok(v), _)) => {
                tracing::debug!("[VOLO] new connection from make_transport for {:?}", key);
                // FIXME: maybe remove waiter
                Ok(self.pooled(&key, v))
            }
            // means connection pool is dropped
            Either::Left((Err(e), _)) => {
                let e = e.into();
                tracing::error!("[VOLO] wait a idle connection error: {:?}", e);
                Err(e)
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

    fn pooled(&self, key: &Key, value: T) -> Pooled<Key, T> {
        let (value, pool_ref) = {
            match value.reserve() {
                Reservation::Shared(to_insert, to_return) => {
                    let mut inner = self.inner.lock().unwrap();
                    inner.put(key.clone(), to_insert);
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
        Pooled::new(key.clone(), value, pool_ref)
    }

    fn reuse(&self, key: &Key, value: T) -> Pooled<Key, T> {
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
        Pooled::new(key.clone(), value, pool_ref)
    }
}

struct Idle<T> {
    inner: T,
    idle_at: Instant,
}

#[pin_project]
pub struct Pooled<Key, T>
where
    Key: Eq + Hash + Debug + 'static + Send,
    T: Poolable + 'static + Send,
{
    key: Option<Key>,
    #[pin]
    t: Option<T>,
    // shared transport no need pool ref
    pool: Option<Weak<Mutex<Inner<Key, T>>>>,
}

impl<Key, T> Pooled<Key, T>
where
    T: Poolable + Send,
    Key: Eq + Hash + Debug + Send,
{
    fn new(key: Key, t: T, pool: Option<Weak<Mutex<Inner<Key, T>>>>) -> Self {
        Pooled {
            key: Some(key),
            t: Some(t),
            pool,
        }
    }

    pub(crate) fn reuse(mut self) {
        let inner = self.t.take().volo_unwrap();
        if !inner.reusable() {
            // If we *already* know the connection is done here,
            // it shouldn't be re-inserted back into the pool.
            return;
        }
        let pool = self.pool.clone();
        let key = self.key.take().volo_unwrap();
        if let Some(pool) = pool {
            if let Some(pool) = pool.upgrade() {
                if let Ok(mut pool) = pool.lock() {
                    pool.put(key, inner);
                }
            }
        }
    }
}

impl<Key, T> AsRef<T> for Pooled<Key, T>
where
    Key: Eq + Hash + Debug + Send,
    T: Poolable + Send,
{
    fn as_ref(&self) -> &T {
        self.t.as_ref().expect("not dropped")
    }
}

impl<Key, T> AsMut<T> for Pooled<Key, T>
where
    Key: Eq + Hash + Debug + Send,
    T: Poolable + Send,
{
    fn as_mut(&mut self) -> &mut T {
        self.t.as_mut().expect("not dropped")
    }
}

impl<Key, T> Deref for Pooled<Key, T>
where
    T: Poolable + Send,
    Key: Eq + Hash + Debug + Send,
{
    type Target = T;
    fn deref(&self) -> &T {
        self.as_ref()
    }
}

impl<Key, T> DerefMut for Pooled<Key, T>
where
    T: Poolable + Send,
    Key: Eq + Hash + Debug + Send,
{
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

struct Inner<Key, T> {
    // idle queue
    idle: HashMap<Key, Vec<Idle<T>>>,
    // waiters wait for idle transport
    waiters: HashMap<Key, WaiterList<T>>,
    // idle timeout and check interval
    timeout: Duration,
    // idle count per key
    max_idle_per_key: usize,
    // when rx dropped, then tx poll_closed will return Poll::Ready(())
    // then idle task exist
    _pool_drop_rx: oneshot::Receiver<()>,
}

impl<Key, T: Poolable> Inner<Key, T>
where
    Key: Eq + Hash + Debug,
{
    // clear expired idle
    fn clear_expired(&mut self) {
        let timeout = self.timeout;
        let now = Instant::now();
        self.idle.retain(|key, values| {
            values.retain(|entry| {
                // TODO: check has_idle && remove the (idle, waiters) key
                if !entry.inner.reusable() {
                    tracing::trace!("[VOLO] idle interval evicting closed for {:?}", key);
                    return false;
                }
                if now - entry.idle_at > timeout {
                    tracing::trace!("[VOLO] idle interval evicting expired for {:?}", key);
                    return false;
                }

                true
            });
            !values.is_empty()
        });
    }
}

impl<Key, T> Inner<Key, T>
where
    Key: Eq + Hash + Debug,
    T: Poolable,
{
    fn put(&mut self, key: Key, t: T) {
        if t.can_share() && self.idle.contains_key(&key) {
            tracing::trace!(
                "[VOLO] put; existing idle Shareable connection for {:?}",
                key
            );
            return;
        }
        // check the wait queue
        let mut value = Some(t);
        if let Some(waiters) = self.waiters.get_mut(&key) {
            // find a waiter and send
            while let Some(waiter) = waiters.pop() {
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
                            break;
                        }
                        Err(t) => {
                            value = Some(t);
                        }
                    }
                }
            }
            // if waiters is empty then remove from waiters
            if waiters.is_empty() {
                self.waiters.remove(&key);
            }
        }
        // check if send to some waiter
        if let Some(t) = value {
            // means doesn't send success
            // then put back to idle list
            let idle = self.idle.entry(key).or_insert_with(Vec::new);
            if idle.len() < self.max_idle_per_key {
                idle.push(Idle {
                    inner: t,
                    idle_at: Instant::now(),
                });
            }
        }
    }
}

// Idle refresh task
#[pin_project]
struct IdleTask<Key, T> {
    // refresh interval
    #[pin]
    interval: Interval,
    // pool
    inner: Weak<Mutex<Inner<Key, T>>>,
    // drop tx and rx recv error
    #[pin]
    pool_drop_tx: oneshot::Sender<()>,
}

impl<Key, T> Future for IdleTask<Key, T>
where
    Key: Eq + Hash + Debug,
    T: Poolable,
{
    type Output = ();

    // long loop for check transport timeout
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        loop {
            match this.pool_drop_tx.as_mut().poll_closed(cx) {
                Poll::Ready(()) => {
                    tracing::trace!("[VOLO] pool closed, canceling idle interval");
                    return Poll::Ready(());
                }
                Poll::Pending => (),
            }
            ready!(this.interval.as_mut().poll_tick(cx));
            if let Some(inner) = this.inner.upgrade() {
                if let Ok(mut inner) = inner.lock() {
                    tracing::trace!("[VOLO] idle interval checking for expired");
                    inner.clear_expired();

                    continue;
                }
            }
            return Poll::Ready(());
        }
    }
}
