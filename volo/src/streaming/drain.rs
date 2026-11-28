//! Connection-level shutdown coordination. Native drivers own calls and tasks.
use std::{
    future::Future,
    sync::{Arc, Weak},
};

use futures::future::BoxFuture;
use parking_lot::Mutex;

/// Implemented by the existing native connection owner, never by a second RPC registry.
#[doc(hidden)]
pub trait NativeConnection: Send + Sync + 'static {
    fn cancel(&self, error: super::RpcError);
    fn active_calls(&self) -> usize;
    fn active_tasks(&self) -> usize;
    fn reclaimed(&self) -> BoxFuture<'_, ()>;
}

#[derive(Clone, Default)]
#[doc(hidden)]
pub struct ServerDrain(Arc<Mutex<Connections>>);
#[derive(Default)]
struct Connections {
    live: Vec<Connection>,
    forced: Option<super::RpcError>,
}
struct Connection {
    driver: Weak<dyn NativeConnection>,
    canceled: bool,
    claimed: bool,
}
tokio::task_local! { static CURRENT: ServerDrain; }
impl ServerDrain {
    pub fn register(connection: Arc<dyn NativeConnection>) {
        let forced = CURRENT
            .try_with(|drain| {
                let mut state = drain.0.lock();
                // Amortize cleanup instead of scanning all connections on each accept.
                if state.live.len() == state.live.capacity() {
                    state.live.retain(|entry| entry.driver.strong_count() != 0);
                    if state.live.len() == state.live.capacity() {
                        let additional = state.live.len().max(16);
                        state.live.reserve(additional);
                    }
                }
                let canceled = state.forced.is_some();
                state.live.push(Connection {
                    driver: Arc::downgrade(&connection),
                    canceled,
                    claimed: false,
                });
                state.forced.clone()
            })
            .ok()
            .flatten();
        if let Some(error) = forced {
            connection.cancel(error);
        }
    }
    pub fn coordinated() -> bool {
        CURRENT.try_with(|_| ()).is_ok()
    }
    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        CURRENT.scope(self.clone(), future).await
    }
    fn connections(&self) -> Vec<Arc<dyn NativeConnection>> {
        self.0
            .lock()
            .live
            .iter()
            .filter_map(|entry| entry.driver.upgrade())
            .collect()
    }
    pub fn active_calls(&self) -> usize {
        self.connections().iter().map(|c| c.active_calls()).sum()
    }
    pub fn active_tasks(&self) -> usize {
        self.connections().iter().map(|c| c.active_tasks()).sum()
    }
    /// Snapshot once at shutdown; normal RPCs never acquire the server-wide lock.
    pub fn cancel_all(&self) -> Vec<Arc<dyn NativeConnection>> {
        let (connections, error) = {
            let mut state = self.0.lock();
            let error = state
                .forced
                .get_or_insert_with(|| super::RpcError::Canceled {
                    cause: super::CancelCause::ServerShutdown,
                    code: 0,
                    message: "server shutdown deadline or run owner dropped".into(),
                    path: Vec::new(),
                })
                .clone();
            (
                state
                    .live
                    .iter_mut()
                    .filter_map(|entry| {
                        if entry.claimed {
                            return None;
                        }
                        entry.claimed = true;
                        let cancel = !entry.canceled;
                        entry.canceled = true;
                        entry.driver.upgrade().map(|driver| (driver, cancel))
                    })
                    .collect::<Vec<_>>(),
                error,
            )
        };
        for (connection, cancel) in &connections {
            if *cancel {
                connection.cancel(error.clone());
            }
        }
        connections.into_iter().map(|(driver, _)| driver).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Probe(
        Mutex<Option<crate::streaming::RpcError>>,
        std::sync::atomic::AtomicUsize,
    );
    impl NativeConnection for Probe {
        fn cancel(&self, error: crate::streaming::RpcError) {
            self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            *self.0.lock() = Some(error);
        }
        fn active_calls(&self) -> usize {
            3
        }
        fn active_tasks(&self) -> usize {
            4
        }
        fn reclaimed(&self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }
    #[tokio::test]
    async fn native_snapshots_and_late_connections_share_the_shutdown_cause() {
        let drain = ServerDrain::default();
        let first = Arc::new(Probe::default());
        drain
            .scope(async {
                ServerDrain::register(first.clone());
            })
            .await;
        assert_eq!((drain.active_calls(), drain.active_tasks()), (3, 4));
        assert_eq!(drain.cancel_all().len(), 1);
        let late = Arc::new(Probe::default());
        drain
            .scope(async {
                ServerDrain::register(late.clone());
            })
            .await;
        for connection in [&first, &late] {
            assert!(matches!(
                &*connection.0.lock(),
                Some(crate::streaming::RpcError::Canceled {
                    cause: crate::streaming::CancelCause::ServerShutdown,
                    ..
                })
            ));
        }
        assert_eq!(
            drain.cancel_all().len(),
            1,
            "only the late driver needs reclaiming"
        );
        assert!(drain.cancel_all().is_empty());
        assert_eq!(first.1.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(late.1.load(std::sync::atomic::Ordering::Relaxed), 1);
        drop(first);
        drop(late);
        assert_eq!((drain.active_calls(), drain.active_tasks()), (0, 0));
    }
}
