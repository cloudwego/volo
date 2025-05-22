// These codes are originally copied from `hyper/client/pool.rs` with some modifications.

use std::{
    collections::VecDeque,
    convert::Infallible,
    fmt,
    fmt::Debug,
    future::Future,
    hash::Hash,
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::{Arc, Weak},
    task::{ready, Context, Poll},
    time::Duration,
};

use ahash::{AHashMap, AHashSet};
use http::Version;
use parking_lot::Mutex;
use pin_project::pin_project;
use tokio::{sync::oneshot, time::Instant};

pub struct Pool<K: Key, T> {
    // If the pool is disabled, this is None.
    inner: Arc<Mutex<PoolInner<K, T>>>,
}

// Before using a pooled connection, make sure the sender is not dead.
//
// This is a trait to allow the `client::pool::tests` to work for `i32`.
//
// See https://github.com/hyperium/hyper/issues/1429
pub trait Poolable: Unpin + Send + Sized + 'static {
    fn is_open(&self) -> bool;
    /// Reserve this connection.
    ///
    /// Allows for HTTP/2 to return a shared reservation.
    fn reserve(self) -> Reservation<Self>;
    fn can_share(&self) -> bool;
}

pub trait Key: Eq + Hash + Clone + Debug + Unpin + Send + 'static {}

impl<T> Key for T where T: Eq + Hash + Clone + Debug + Unpin + Send + 'static {}

/// A marker to identify what version a pooled connection is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ver {
    Auto,
    #[cfg(feature = "http2")]
    Http2,
}

impl From<Version> for Ver {
    fn from(_value: Version) -> Self {
        #[cfg(feature = "http2")]
        if _value == Version::HTTP_2 {
            return Ver::Http2;
        }
        Ver::Auto
    }
}

/// When checking out a pooled connection, it might be that the connection
/// only supports a single reservation, or it might be usable for many.
///
/// Specifically, HTTP/1 requires a unique reservation, but HTTP/2 can be
/// used for multiple requests.
pub enum Reservation<T> {
    /// This connection could be used multiple times, the first one will be
    /// reinserted into the `idle` pool, and the second will be given to
    /// the `Checkout`.
    #[cfg(feature = "http2")]
    Shared(T, T),
    /// This connection requires unique access. It will be returned after
    /// use is complete.
    #[cfg(feature = "http1")]
    Unique(T),
}

struct PoolInner<K: Eq + Hash, T> {
    // A flag that a connection is being established, and the connection
    // should be shared. This prevents making multiple HTTP/2 connections
    // to the same host.
    connecting: AHashSet<K>,
    // These are internal Conns sitting in the event loop in the KeepAlive
    // state, waiting to receive a new Request to send on the socket.
    idle: AHashMap<K, Vec<Idle<T>>>,
    max_idle_per_host: usize,
    // These are outstanding Checkouts that are waiting for a socket to be
    // able to send a Request one. This is used when "racing" for a new
    // connection.
    //
    // The Client starts 2 tasks, 1 to connect a new socket, and 1 to wait
    // for the Pool to receive an idle Conn. When a Conn becomes idle,
    // this list is checked for any parked Checkouts, and tries to notify
    // them that the Conn could be used instead of waiting for a brand new
    // connection.
    waiters: AHashMap<K, VecDeque<oneshot::Sender<T>>>,
    // A oneshot channel is used to allow the interval to be notified when
    // the Pool completely drops. That way, the interval can cancel immediately.
    idle_interval_ref: Option<oneshot::Sender<Infallible>>,
    timeout: Duration,
}

// This is because `Weak::new()` *allocates* space for `T`, even if it
// doesn't need it!
struct WeakOpt<T>(Option<Weak<T>>);

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub idle_timeout: Duration,
    pub max_idle_per_host: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(20),
            max_idle_per_host: 10240,
        }
    }
}

impl<K: Key, T> Pool<K, T> {
    pub fn new(config: Config) -> Pool<K, T> {
        let inner = PoolInner {
            connecting: AHashSet::new(),
            idle: AHashMap::new(),
            idle_interval_ref: None,
            max_idle_per_host: config.max_idle_per_host,
            waiters: AHashMap::new(),
            timeout: config.idle_timeout,
        };
        let inner = Arc::new(Mutex::new(inner));

        Pool { inner }
    }

    #[cfg(test)]
    pub(super) fn no_timer(&self) {
        // Prevent an actual interval from being created for this pool...
        {
            let mut inner = self.inner.lock();
            assert!(inner.idle_interval_ref.is_none(), "timer already spawned");
            let (tx, _) = oneshot::channel();
            inner.idle_interval_ref = Some(tx);
        }
    }
}

impl<K: Key, T: Poolable> Pool<K, T> {
    /// Returns a `Checkout` which is a future that resolves if an idle
    /// connection becomes available.
    pub fn checkout(&self, key: K) -> Checkout<K, T> {
        Checkout {
            key,
            pool: self.clone(),
            waiter: None,
        }
    }

    /// Ensure that there is only ever 1 connecting task for HTTP/2
    /// connections. This does nothing for HTTP/1.
    pub fn connecting(&self, key: &K, _ver: Ver) -> Option<Connecting<K, T>> {
        #[cfg(feature = "http2")]
        if _ver == Ver::Http2 {
            let mut inner = self.inner.lock();
            return if inner.connecting.insert(key.clone()) {
                let connecting = Connecting {
                    key: key.clone(),
                    pool: WeakOpt::downgrade(&self.inner),
                };
                Some(connecting)
            } else {
                tracing::trace!("HTTP/2 connecting already in progress for {:?}", key);
                None
            };
        }

        // else
        Some(Connecting {
            key: key.clone(),
            // in HTTP/1's case, there is never a lock, so we don't
            // need to do anything in Drop.
            pool: WeakOpt::none(),
        })
    }

    #[cfg(test)]
    fn locked(&self) -> parking_lot::MutexGuard<'_, PoolInner<K, T>> {
        self.inner.lock()
    }

    pub fn pooled(&self, connecting: Connecting<K, T>, value: T) -> Pooled<K, T> {
        #[cfg(feature = "http2")]
        let mut connecting = connecting;

        let (value, pool_ref) = match value.reserve() {
            #[cfg(feature = "http2")]
            Reservation::Shared(to_insert, to_return) => {
                let mut inner = self.inner.lock();
                inner.put(connecting.key.clone(), to_insert, &self.inner);
                // Do this here instead of Drop for Connecting because we
                // already have a lock, no need to lock the mutex twice.
                inner.connected(&connecting.key);
                // prevent the Drop of Connecting from repeating inner.connected()
                connecting.pool = WeakOpt::none();

                // Shared reservations don't need a reference to the pool,
                // since the pool always keeps a copy.
                (to_return, WeakOpt::none())
            }
            #[cfg(feature = "http1")]
            Reservation::Unique(value) => {
                // Unique reservations must take a reference to the pool
                // since they hope to reinsert once the reservation is
                // completed
                (value, WeakOpt::downgrade(&self.inner))
            }
        };
        Pooled {
            key: connecting.key.clone(),
            pool: pool_ref,
            value: Some(value),
        }
    }

    fn reuse(&self, key: &K, value: T) -> Pooled<K, T> {
        tracing::debug!("reuse idle connection for {:?}", key);
        // TODO(hyper-util): unhack this
        // In Pool::pooled(), which is used for inserting brand new connections,
        // there's some code that adjusts the pool reference taken depending
        // on if the Reservation can be shared or is unique. By the time
        // reuse() is called, the reservation has already been made, and
        // we just have the final value, without knowledge of if this is
        // unique or shared. So, the hack is to just assume Ver::Http2 means
        // shared... :(
        let mut pool_ref = WeakOpt::none();
        if !value.can_share() {
            pool_ref = WeakOpt::downgrade(&self.inner);
        }

        Pooled {
            key: key.clone(),
            pool: pool_ref,
            value: Some(value),
        }
    }
}

/// Pop off this list, looking for a usable connection that hasn't expired.
struct IdlePopper<'a, K, T> {
    key: &'a K,
    list: &'a mut Vec<Idle<T>>,
}

impl<'a, T: Poolable + 'a, K: Debug> IdlePopper<'a, K, T> {
    fn pop(self, expiration: &Expiration) -> Option<Idle<T>> {
        while let Some(entry) = self.list.pop() {
            // If the connection has been closed, or is older than our idle
            // timeout, simply drop it and keep looking...
            if !entry.value.is_open() {
                tracing::trace!("removing closed connection for {:?}", self.key);
                continue;
            }
            // TODO(hyper-util): Actually, since the `idle` list is pushed to
            // the end always, that would imply that if *this* entry is expired,
            // then anything "earlier" in the list would *have* to be expired
            // also... Right?
            //
            // In that case, we could just break out of the loop and drop the
            // whole list...
            if expiration.expires(entry.idle_at) {
                tracing::trace!("removing expired connection for {:?}", self.key);
                continue;
            }

            // The clippy warning will be thrown if `http2` is disabled, but we cannot fix it, just
            // allow the rule.
            #[allow(clippy::infallible_destructuring_match)]
            let value = match entry.value.reserve() {
                #[cfg(feature = "http2")]
                Reservation::Shared(to_reinsert, to_checkout) => {
                    self.list.push(Idle {
                        idle_at: Instant::now(),
                        value: to_reinsert,
                    });
                    to_checkout
                }
                #[cfg(feature = "http1")]
                Reservation::Unique(unique) => unique,
            };

            return Some(Idle {
                idle_at: entry.idle_at,
                value,
            });
        }

        None
    }
}

impl<K: Key, T: Poolable> PoolInner<K, T> {
    fn put(&mut self, key: K, value: T, __pool_ref: &Arc<Mutex<PoolInner<K, T>>>) {
        if value.can_share() && self.idle.contains_key(&key) {
            tracing::trace!("put; existing idle HTTP/2 connection for {:?}", key);
            return;
        }
        tracing::trace!("put; add idle connection for {:?}", key);
        let mut remove_waiters = false;
        let mut value = Some(value);
        if let Some(waiters) = self.waiters.get_mut(&key) {
            while let Some(tx) = waiters.pop_front() {
                if !tx.is_closed() {
                    let reserved = value.take().expect("value already sent");
                    #[allow(clippy::infallible_destructuring_match)]
                    let reserved = match reserved.reserve() {
                        #[cfg(feature = "http2")]
                        Reservation::Shared(to_keep, to_send) => {
                            value = Some(to_keep);
                            to_send
                        }
                        #[cfg(feature = "http1")]
                        Reservation::Unique(uniq) => uniq,
                    };
                    match tx.send(reserved) {
                        Ok(()) => {
                            if value.is_none() {
                                break;
                            } else {
                                continue;
                            }
                        }
                        Err(e) => {
                            value = Some(e);
                        }
                    }
                }

                tracing::trace!("put; removing canceled waiter for {:?}", key);
            }
            remove_waiters = waiters.is_empty();
        }
        if remove_waiters {
            self.waiters.remove(&key);
        }

        match value {
            Some(value) => {
                // borrow-check scope...
                {
                    let idle_list = self.idle.entry(key.clone()).or_default();
                    if self.max_idle_per_host <= idle_list.len() {
                        tracing::trace!("max idle per host for {:?}, dropping connection", key);
                        return;
                    }

                    tracing::debug!("pooling idle connection for {:?}", key);
                    idle_list.push(Idle {
                        value,
                        idle_at: Instant::now(),
                    });
                }

                self.spawn_idle_interval(__pool_ref);
            }
            None => tracing::trace!("put; found waiter for {:?}", key),
        }
    }

    /// A `Connecting` task is complete. Not necessarily successfully,
    /// but the lock is going away, so clean up.
    fn connected(&mut self, key: &K) {
        let existed = self.connecting.remove(key);
        debug_assert!(existed, "Connecting dropped, key not in pool.connecting");
        // cancel any waiters. if there are any, it's because
        // this Connecting task didn't complete successfully.
        // those waiters would never receive a connection.
        self.waiters.remove(key);
    }

    fn spawn_idle_interval(&mut self, pool_ref: &Arc<Mutex<PoolInner<K, T>>>) {
        if self.idle_interval_ref.is_some() {
            return;
        }
        let (tx, rx) = oneshot::channel();
        self.idle_interval_ref = Some(tx);

        let interval = IdleTask {
            duration: self.timeout,
            deadline: Instant::now(),
            fut: Box::pin(tokio::time::sleep_until(Instant::now())), // ready at first tick
            pool: WeakOpt::downgrade(pool_ref),
            pool_drop_notifier: rx,
        };

        tokio::spawn(interval);
    }
}

impl<K: Eq + Hash, T> PoolInner<K, T> {
    /// Any `FutureResponse`s that were created will have made a `Checkout`,
    /// and possibly inserted into the pool that it is waiting for an idle
    /// connection. If a user ever dropped that future, we need to clean out
    /// those parked senders.
    fn clean_waiters(&mut self, key: &K) {
        let mut remove_waiters = false;
        if let Some(waiters) = self.waiters.get_mut(key) {
            waiters.retain(|tx| !tx.is_closed());
            remove_waiters = waiters.is_empty();
        }
        if remove_waiters {
            self.waiters.remove(key);
        }
    }
}

impl<K: Key, T: Poolable> PoolInner<K, T> {
    /// This should *only* be called by the IdleTask
    fn clear_expired(&mut self) {
        let now = Instant::now();
        // self.last_idle_check_at = now;

        self.idle.retain(|key, values| {
            values.retain(|entry| {
                if !entry.value.is_open() {
                    tracing::trace!("idle interval evicting closed for {:?}", key);
                    return false;
                }

                // Avoid `Instant::sub` to avoid issues like rust-lang/rust#86470.
                if now.saturating_duration_since(entry.idle_at) > self.timeout {
                    tracing::trace!("idle interval evicting expired for {:?}", key);
                    return false;
                }

                // Otherwise, keep this value...
                true
            });

            // returning false evicts this key/val
            !values.is_empty()
        });
    }
}

impl<K: Key, T> Clone for Pool<K, T> {
    fn clone(&self) -> Pool<K, T> {
        Pool {
            inner: self.inner.clone(),
        }
    }
}

/// A wrapped poolable value that tries to reinsert to the Pool on Drop.
// Note: The bounds `T: Poolable` is needed for the Drop impl.
pub struct Pooled<K: Key, T: Poolable> {
    value: Option<T>,
    key: K,
    pool: WeakOpt<Mutex<PoolInner<K, T>>>,
}

impl<K: Key, T: Poolable> Pooled<K, T> {
    fn as_ref(&self) -> &T {
        self.value.as_ref().expect("not dropped")
    }

    fn as_mut(&mut self) -> &mut T {
        self.value.as_mut().expect("not dropped")
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

impl<K: Key, T: Poolable> Drop for Pooled<K, T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            if !value.is_open() {
                // If we *already* know the connection is done here,
                // it shouldn't be re-inserted back into the pool.
                return;
            }

            if let Some(pool) = self.pool.upgrade() {
                pool.lock().put(self.key.clone(), value, &pool);
            } else if !value.can_share() {
                tracing::trace!("pool dropped, dropping pooled ({:?})", self.key);
            }
            // Ver::Http2 is already in the Pool (or dead), so we wouldn't
            // have an actual reference to the Pool.
        }
    }
}

impl<K: Key, T: Poolable> fmt::Debug for Pooled<K, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pooled").field("key", &self.key).finish()
    }
}

struct Idle<T> {
    idle_at: Instant,
    value: T,
}

pub struct Checkout<K: Key, T> {
    key: K,
    pool: Pool<K, T>,
    waiter: Option<oneshot::Receiver<T>>,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    CheckoutNoLongerWanted,
    CheckedOutClosedValue,
}

impl Error {
    pub(super) fn is_canceled(&self) -> bool {
        matches!(self, Error::CheckedOutClosedValue)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Error::CheckedOutClosedValue => "checked out connection was closed",
            Error::CheckoutNoLongerWanted => "request was canceled",
        })
    }
}

impl std::error::Error for Error {}

impl<K: Key, T: Poolable> Checkout<K, T> {
    fn poll_waiter(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Pooled<K, T>, Error>>> {
        if let Some(mut rx) = self.waiter.take() {
            match Pin::new(&mut rx).poll(cx) {
                Poll::Ready(Ok(value)) => {
                    if value.is_open() {
                        Poll::Ready(Some(Ok(self.pool.reuse(&self.key, value))))
                    } else {
                        Poll::Ready(Some(Err(Error::CheckedOutClosedValue)))
                    }
                }
                Poll::Pending => {
                    self.waiter = Some(rx);
                    Poll::Pending
                }
                Poll::Ready(Err(_canceled)) => {
                    Poll::Ready(Some(Err(Error::CheckoutNoLongerWanted)))
                }
            }
        } else {
            Poll::Ready(None)
        }
    }

    fn checkout(&mut self, cx: &mut Context<'_>) -> Option<Pooled<K, T>> {
        let entry = {
            let mut inner = self.pool.inner.lock();
            let expiration = Expiration::new(inner.timeout);
            let maybe_entry = inner.idle.get_mut(&self.key).and_then(|list| {
                tracing::trace!("take? {:?}: expiration = {:?}", self.key, expiration.0);
                // A block to end the mutable borrow on list,
                // so the map below can check is_empty()
                {
                    let popper = IdlePopper {
                        key: &self.key,
                        list,
                    };
                    popper.pop(&expiration)
                }
                .map(|e| (e, list.is_empty()))
            });

            let (entry, empty) = if let Some((e, empty)) = maybe_entry {
                (Some(e), empty)
            } else {
                // No entry found means nuke the list for sure.
                (None, true)
            };
            if empty {
                // TODO(hyper-util): This could be done with the HashMap::entry API instead.
                inner.idle.remove(&self.key);
            }

            if entry.is_none() && self.waiter.is_none() {
                let (tx, mut rx) = oneshot::channel();
                tracing::trace!("checkout waiting for idle connection: {:?}", self.key);
                inner
                    .waiters
                    .entry(self.key.clone())
                    .or_default()
                    .push_back(tx);

                // register the waker with this oneshot
                assert!(Pin::new(&mut rx).poll(cx).is_pending());
                self.waiter = Some(rx);
            }

            entry
        };

        entry.map(|e| self.pool.reuse(&self.key, e.value))
    }
}

impl<K: Key, T: Poolable> Future for Checkout<K, T> {
    type Output = Result<Pooled<K, T>, Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(pooled) = ready!(self.poll_waiter(cx)?) {
            return Poll::Ready(Ok(pooled));
        }

        if let Some(pooled) = self.checkout(cx) {
            Poll::Ready(Ok(pooled))
        } else {
            // There's a new waiter, already registered in self.checkout()
            debug_assert!(self.waiter.is_some());
            Poll::Pending
        }
    }
}

impl<K: Key, T> Drop for Checkout<K, T> {
    fn drop(&mut self) {
        if self.waiter.take().is_some() {
            tracing::trace!("checkout dropped for {:?}", self.key);
            self.pool.inner.lock().clean_waiters(&self.key);
        }
    }
}

pub struct Connecting<K: Key, T: Poolable> {
    key: K,
    pool: WeakOpt<Mutex<PoolInner<K, T>>>,
}

#[cfg(feature = "http2")]
impl<K: Key, T: Poolable> Connecting<K, T> {
    pub fn alpn_h2(self, pool: &Pool<K, T>) -> Option<Self> {
        debug_assert!(
            self.pool.0.is_none(),
            "Connecting::alpn_h2 but already Http2"
        );

        pool.connecting(&self.key, Ver::Http2)
    }
}

impl<K: Key, T: Poolable> Drop for Connecting<K, T> {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade() {
            // No need to panic on drop, that could abort!
            pool.lock().connected(&self.key);
        }
    }
}

struct Expiration(Duration);

impl Expiration {
    fn new(dur: Duration) -> Expiration {
        Expiration(dur)
    }

    fn expires(&self, instant: Instant) -> bool {
        // Avoid `Instant::elapsed` to avoid issues like rust-lang/rust#86470.
        Instant::now().saturating_duration_since(instant) > self.0
    }
}

#[pin_project]
struct IdleTask<K: Key, T> {
    duration: Duration,
    deadline: Instant,
    fut: Pin<Box<tokio::time::Sleep>>,
    pool: WeakOpt<Mutex<PoolInner<K, T>>>,
    // This allows the IdleTask to be notified as soon as the entire
    // Pool is fully dropped, and shutdown. This channel is never sent on,
    // but Err(Canceled) will be received when the Pool is dropped.
    #[pin]
    pool_drop_notifier: oneshot::Receiver<Infallible>,
}

impl<T: Poolable + 'static, K: Key> Future for IdleTask<K, T> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        loop {
            match this.pool_drop_notifier.as_mut().poll(cx) {
                Poll::Ready(Ok(n)) => match n {},
                Poll::Pending => (),
                Poll::Ready(Err(_canceled)) => {
                    tracing::trace!("pool closed, canceling idle interval");
                    return Poll::Ready(());
                }
            }

            ready!(this.fut.as_mut().poll(cx));
            // Set this task to run after the next deadline
            // If the poll missed the deadline by a lot, set the deadline
            // from the current time instead
            *this.deadline += *this.duration;
            if *this.deadline < Instant::now() - Duration::from_millis(5) {
                *this.deadline = Instant::now() + *this.duration;
            }
            *this.fut = Box::pin(tokio::time::sleep_until(*this.deadline));

            if let Some(inner) = this.pool.upgrade() {
                tracing::trace!("idle interval checking for expired");
                inner.lock().clear_expired();
                continue;
            }
            return Poll::Ready(());
        }
    }
}

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

#[cfg(test)]
#[cfg(feature = "http1")]
mod tests {
    use std::{
        fmt::Debug,
        future::Future,
        hash::Hash,
        pin::Pin,
        task::{self, Poll},
        time::Duration,
    };

    use super::{Connecting, Key, Pool, Poolable, Reservation, WeakOpt};

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    struct KeyImpl(http::uri::Scheme, http::uri::Authority);

    /// Test unique reservations.
    #[derive(Debug, PartialEq, Eq)]
    struct Uniq<T>(T);

    impl<T: Send + 'static + Unpin> Poolable for Uniq<T> {
        fn is_open(&self) -> bool {
            true
        }

        fn reserve(self) -> Reservation<Self> {
            Reservation::Unique(self)
        }

        fn can_share(&self) -> bool {
            false
        }
    }

    fn c<K: Key, T: Poolable>(key: K) -> Connecting<K, T> {
        Connecting {
            key,
            pool: WeakOpt::none(),
        }
    }

    fn host_key(s: &str) -> KeyImpl {
        KeyImpl(http::uri::Scheme::HTTP, s.parse().expect("host key"))
    }

    fn pool_no_timer<K: Key, T>() -> Pool<K, T> {
        pool_max_idle_no_timer(usize::MAX)
    }

    fn pool_max_idle_no_timer<K: Key, T>(max_idle: usize) -> Pool<K, T> {
        let pool = Pool::new(super::Config {
            idle_timeout: Duration::from_millis(100),
            max_idle_per_host: max_idle,
        });
        pool.no_timer();
        pool
    }

    #[tokio::test]
    async fn test_pool_checkout_smoke() {
        let pool = pool_no_timer();
        let key = host_key("foo");
        let pooled = pool.pooled(c(key.clone()), Uniq(41));

        drop(pooled);

        match pool.checkout(key).await {
            Ok(pooled) => assert_eq!(*pooled, Uniq(41)),
            Err(_) => panic!("not ready"),
        };
    }

    /// Helper to check if the future is ready after polling once.
    struct PollOnce<'a, F>(&'a mut F);

    impl<F, T, U> Future for PollOnce<'_, F>
    where
        F: Future<Output = Result<T, U>> + Unpin,
    {
        type Output = Option<()>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
            match Pin::new(&mut self.0).poll(cx) {
                Poll::Ready(Ok(_)) => Poll::Ready(Some(())),
                Poll::Ready(Err(_)) => Poll::Ready(Some(())),
                Poll::Pending => Poll::Ready(None),
            }
        }
    }

    #[tokio::test]
    async fn test_pool_checkout_returns_none_if_expired() {
        let pool = pool_no_timer();
        let key = host_key("foo");
        let pooled = pool.pooled(c(key.clone()), Uniq(41));

        drop(pooled);
        let timeout = pool.locked().timeout;
        tokio::time::sleep(timeout).await;
        let mut checkout = pool.checkout(key);
        let poll_once = PollOnce(&mut checkout);
        let is_not_ready = poll_once.await.is_none();
        assert!(is_not_ready);
    }

    #[tokio::test]
    async fn test_pool_checkout_removes_expired() {
        let pool = pool_no_timer();
        let key = host_key("foo");

        pool.pooled(c(key.clone()), Uniq(41));
        pool.pooled(c(key.clone()), Uniq(5));
        pool.pooled(c(key.clone()), Uniq(99));

        assert_eq!(
            pool.locked().idle.get(&key).map(|entries| entries.len()),
            Some(3)
        );
        let timeout = pool.locked().timeout;
        tokio::time::sleep(timeout).await;

        let mut checkout = pool.checkout(key.clone());
        let poll_once = PollOnce(&mut checkout);
        // checkout.await should clean out the expired
        poll_once.await;
        assert!(pool.locked().idle.get(&key).is_none());
    }

    #[test]
    fn test_pool_max_idle_per_host() {
        let pool = pool_max_idle_no_timer(2);
        let key = host_key("foo");

        pool.pooled(c(key.clone()), Uniq(41));
        pool.pooled(c(key.clone()), Uniq(5));
        pool.pooled(c(key.clone()), Uniq(99));

        // pooled and dropped 3, max_idle should only allow 2
        assert_eq!(
            pool.locked().idle.get(&key).map(|entries| entries.len()),
            Some(2)
        );
    }

    #[tokio::test]
    async fn test_pool_timer_removes_expired() {
        let pool = Pool::new(super::Config {
            idle_timeout: Duration::from_millis(10),
            max_idle_per_host: usize::MAX,
        });

        let key = host_key("foo");

        pool.pooled(c(key.clone()), Uniq(41));
        pool.pooled(c(key.clone()), Uniq(5));
        pool.pooled(c(key.clone()), Uniq(99));

        assert_eq!(
            pool.locked().idle.get(&key).map(|entries| entries.len()),
            Some(3)
        );

        // Let the timer tick passed the expiration...
        tokio::time::sleep(Duration::from_millis(30)).await;
        // Yield so the Interval can reap...
        tokio::task::yield_now().await;

        assert!(pool.locked().idle.get(&key).is_none());
    }

    #[tokio::test]
    async fn test_pool_checkout_task_unparked() {
        use futures_util::{future::join, FutureExt};

        let pool = pool_no_timer();
        let key = host_key("foo");
        let pooled = pool.pooled(c(key.clone()), Uniq(41));

        let checkout = join(pool.checkout(key), async {
            // the checkout future will park first,
            // and then this lazy future will be polled, which will insert
            // the pooled back into the pool
            //
            // this test makes sure that doing so will unpark the checkout
            drop(pooled);
        })
        .map(|(entry, _)| entry);

        assert_eq!(*checkout.await.unwrap(), Uniq(41));
    }

    #[tokio::test]
    async fn test_pool_checkout_drop_cleans_up_waiters() {
        let pool = pool_no_timer::<KeyImpl, Uniq<i32>>();
        let key = host_key("foo");

        let mut checkout1 = pool.checkout(key.clone());
        let mut checkout2 = pool.checkout(key.clone());

        let poll_once1 = PollOnce(&mut checkout1);
        let poll_once2 = PollOnce(&mut checkout2);

        // first poll needed to get into Pool's parked
        poll_once1.await;
        assert_eq!(pool.locked().waiters.get(&key).unwrap().len(), 1);
        poll_once2.await;
        assert_eq!(pool.locked().waiters.get(&key).unwrap().len(), 2);

        // on drop, clean up Pool
        drop(checkout1);
        assert_eq!(pool.locked().waiters.get(&key).unwrap().len(), 1);

        drop(checkout2);
        assert!(pool.locked().waiters.get(&key).is_none());
    }

    #[derive(Debug)]
    struct CanClose {
        #[allow(unused)]
        val: i32,
        closed: bool,
    }

    impl Poolable for CanClose {
        fn is_open(&self) -> bool {
            !self.closed
        }

        fn reserve(self) -> Reservation<Self> {
            Reservation::Unique(self)
        }

        fn can_share(&self) -> bool {
            false
        }
    }

    #[test]
    fn pooled_drop_if_closed_doesnt_reinsert() {
        let pool = pool_no_timer();
        let key = host_key("foo");
        pool.pooled(
            c(key.clone()),
            CanClose {
                val: 57,
                closed: true,
            },
        );

        assert!(!pool.locked().idle.contains_key(&key));
    }
}
