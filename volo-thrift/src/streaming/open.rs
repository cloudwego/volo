use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

use motore::{layer::Layer, service::Service};
use parking_lot::{Mutex, RwLock};
use rustc_hash::FxHashMap;
use tokio::sync::Notify;
use volo::net::{Address, dial::MakeTransport};

use super::{
    HookChain, IoPhase, Metadata, OpenedStream, PoolMode, StreamConfig, StreamContext, StreamError,
};
use crate::transport::ttstream::connection::Connection;

#[derive(Clone, Debug, Default)]
pub struct OpenStream {
    pub address: Option<Address>,
    pub metadata: Metadata,
}

type OpenFuture<'a> = Pin<Box<dyn Future<Output = Result<OpenedStream, StreamError>> + Send + 'a>>;
trait DynOpen: Send + Sync {
    fn open<'a>(&'a self, cx: &'a mut StreamContext, request: OpenStream) -> OpenFuture<'a>;
}
struct Adapter<S>(S);
impl<S> DynOpen for Adapter<S>
where
    S: Service<StreamContext, OpenStream, Response = OpenedStream, Error = StreamError>
        + Send
        + Sync
        + 'static,
{
    fn open<'a>(&'a self, cx: &'a mut StreamContext, request: OpenStream) -> OpenFuture<'a> {
        Box::pin(self.0.call(cx, request))
    }
}
/// A single type-erased boundary per stream; messages retain their concrete codec.
#[derive(Clone)]
pub struct OpenService(Arc<dyn DynOpen>);
impl OpenService {
    pub fn new<S>(service: S) -> Self
    where
        S: Service<StreamContext, OpenStream, Response = OpenedStream, Error = StreamError>
            + Send
            + Sync
            + 'static,
    {
        Self(Arc::new(Adapter(service)))
    }
}
impl Service<StreamContext, OpenStream> for OpenService {
    type Response = OpenedStream;
    type Error = StreamError;
    async fn call(
        &self,
        cx: &mut StreamContext,
        request: OpenStream,
    ) -> Result<OpenedStream, StreamError> {
        self.0.open(cx, request).await
    }
}

#[derive(Default)]
pub(crate) struct OpenOptions {
    pub enabled: bool,
    pub config: StreamConfig,
    pub hooks: Option<HookChain>,
    pub service: Option<OpenService>,
    pub layers: Vec<Box<dyn FnOnce(OpenService) -> OpenService + Send>>,
}
impl OpenOptions {
    pub fn layer<L>(&mut self, layer: L)
    where
        L: Layer<OpenService> + Send + 'static,
        L::Service: Service<StreamContext, OpenStream, Response = OpenedStream, Error = StreamError>
            + Send
            + Sync
            + 'static,
    {
        self.layers
            .push(Box::new(move |inner| OpenService::new(layer.layer(inner))));
    }
    pub fn layer_front<L>(&mut self, layer: L)
    where
        L: Layer<OpenService> + Send + 'static,
        L::Service: Service<StreamContext, OpenStream, Response = OpenedStream, Error = StreamError>
            + Send
            + Sync
            + 'static,
    {
        self.layers.insert(
            0,
            Box::new(move |inner| OpenService::new(layer.layer(inner))),
        );
    }
    pub fn build<M: MakeTransport>(
        self,
        transport: M,
        resolver: Option<volo::loadbalance::EndpointResolver>,
    ) -> OpenService {
        let hooks = self.hooks.filter(|hooks| !hooks.is_empty());
        let deadline = self.config.deadline;
        let transport = self.service.unwrap_or_else(|| {
            OpenService::new(TransportOpen {
                transport,
                config: self.config,
                validation: Default::default(),
                hooks: hooks.clone(),
                pool: Arc::default(),
                resolver,
            })
        });
        // Layers may resolve the target before on_open injects outbound metadata.
        let mut service = OpenService::new(OpenHooks { inner: transport });
        for layer in self.layers.into_iter().rev() {
            service = layer(service);
        }
        // This guard encloses custom services and every layer, including failures
        // before TransportOpen is reached. It never retries a submitted stream.
        OpenService::new(OpenBoundary {
            inner: service,
            hooks,
            deadline,
        })
    }
}

struct PoolEntry {
    connection: Connection,
    leases: AtomicUsize,
    idle: Mutex<Instant>,
}
#[derive(Default)]
struct TargetPool {
    entries: Mutex<Vec<Arc<PoolEntry>>>,
    connecting: AtomicUsize,
    wake: Notify,
}
struct Connecting<'a>(&'a TargetPool);
impl Drop for Connecting<'_> {
    fn drop(&mut self) {
        self.0.connecting.fetch_sub(1, Ordering::AcqRel);
        self.0.wake.notify_waiters();
    }
}
impl Drop for TargetPool {
    fn drop(&mut self) {
        for entry in self.entries.get_mut().drain(..) {
            entry.connection.retire();
        }
    }
}
struct Lease {
    entry: Arc<PoolEntry>,
    target: Arc<TargetPool>,
    short: bool,
    max_idle: usize,
}
impl Drop for Lease {
    fn drop(&mut self) {
        *self.entry.idle.lock() = Instant::now();
        if self.entry.leases.fetch_sub(1, Ordering::AcqRel) == 1 {
            let mut entries = self.target.entries.lock();
            let mut idle = 0;
            entries.retain(|entry| {
                if entry.leases.load(Ordering::Acquire) != 0 {
                    return true;
                }
                if (self.short && Arc::ptr_eq(entry, &self.entry)) || entry.connection.is_closed() {
                    entry.connection.retire();
                    return false;
                }
                idle += 1;
                if idle > self.max_idle {
                    entry.connection.retire();
                    return false;
                }
                true
            });
        }
        self.target.wake.notify_waiters();
    }
}
// Endpoint extensions need not be Clone. Move ownership across the async resolver
// boundary and restore it even when acquisition is canceled.
struct EndpointLease {
    cx: StreamContext,
    value: Option<volo::context::Endpoint>,
}
impl EndpointLease {
    fn take(cx: &StreamContext) -> Self {
        Self {
            cx: cx.clone(),
            value: Some(cx.update_rpc_info(|rpc| std::mem::take(rpc.callee_mut()))),
        }
    }
}
impl Drop for EndpointLease {
    fn drop(&mut self) {
        if let Some(endpoint) = self.value.take() {
            self.cx.update_rpc_info(|rpc| *rpc.callee_mut() = endpoint);
        }
    }
}

struct TransportOpen<M> {
    transport: M,
    resolver: Option<volo::loadbalance::EndpointResolver>,
    config: StreamConfig,
    validation: std::sync::OnceLock<Result<(), StreamError>>,
    hooks: Option<HookChain>,
    pool: Arc<RwLock<FxHashMap<Address, Arc<TargetPool>>>>,
}
impl<M: MakeTransport> Service<StreamContext, OpenStream> for TransportOpen<M> {
    type Response = OpenedStream;
    type Error = StreamError;
    async fn call(
        &self,
        cx: &mut StreamContext,
        request: OpenStream,
    ) -> Result<OpenedStream, StreamError> {
        self.acquire(cx, request).await
    }
}

struct OpenHooks {
    inner: OpenService,
}
impl Service<StreamContext, OpenStream> for OpenHooks {
    type Response = OpenedStream;
    type Error = StreamError;
    async fn call(
        &self,
        cx: &mut StreamContext,
        request: OpenStream,
    ) -> Result<OpenedStream, StreamError> {
        let mut request = request;
        let mut metadata = std::mem::take(&mut request.metadata);
        metadata.extend(cx.metadata());
        cx.update_metadata(|current| *current = metadata);
        if let Some(hooks) = cx.inner.hooks.get().cloned() {
            super::StreamHook::on_open(&hooks, cx)?;
        }
        self.inner.call(cx, request).await
    }
}
struct OpenBoundary {
    inner: OpenService,
    hooks: Option<HookChain>,
    deadline: Option<std::time::Duration>,
}
impl Service<StreamContext, OpenStream> for OpenBoundary {
    type Response = OpenedStream;
    type Error = StreamError;
    async fn call(
        &self,
        cx: &mut StreamContext,
        request: OpenStream,
    ) -> Result<OpenedStream, StreamError> {
        if let Some(hooks) = &self.hooks {
            let _ = cx.inner.hooks.set(hooks.for_call());
        }
        let mut attempt = OpenAttempt {
            cx: cx.clone(),
            hooks: cx.inner.hooks.get().cloned(),
            success: false,
        };
        if cx.deadline().is_none() {
            cx.set_deadline(self.deadline.map(|duration| Instant::now() + duration));
        }
        let control = cx.clone();
        let timeout = async {
            match control.deadline() {
                Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                None => std::future::pending().await,
            }
        };
        let result = if !cfg!(feature = "ttstream") {
            Err(StreamError::configuration("ttstream feature is disabled"))
        } else if let Some(error) = control.cancellation() {
            Err(error)
        } else {
            tokio::select! {
                result=self.inner.call(cx,request)=>result,
                error=control.cancelled()=>Err(error),
                _=timeout=>{
                    let error = StreamError::Canceled {cause:super::CancelCause::Deadline,code:12015,message:"stream deadline exceeded while opening".into(),path:Vec::new()};
                    control.cancel(error.clone());
                    Err(control.cancellation().unwrap_or(error))
                }
            }
        };
        let result = result.and_then(|stream| {
            if !Arc::ptr_eq(&stream.context().inner, &cx.inner) {
                return Err(StreamError::configuration(
                    "streaming service must preserve the supplied StreamContext",
                ));
            }
            Ok(stream)
        });
        if let Err(error) = &result {
            attempt.fail(error.clone());
        }
        attempt.success = result.is_ok();
        result
    }
}

struct OpenAttempt {
    cx: StreamContext,
    hooks: Option<HookChain>,
    success: bool,
}
impl OpenAttempt {
    fn fail(&self, error: StreamError) {
        let status = super::StreamStatus {
            error: Some(error.clone()),
            resources: self.cx.resources(),
            ..Default::default()
        };
        if self.cx.finish(status.clone()) {
            if let Some(hooks) = &self.hooks {
                super::StreamHook::on_io_error(
                    hooks,
                    &self.cx,
                    &super::StreamIoError {
                        at: Instant::now(),
                        direction: super::IoDirection::Send,
                        phase: match &error {
                            StreamError::Transport { phase, .. } => *phase,
                            _ => IoPhase::Open,
                        },
                        size: None,
                        error,
                    },
                );
                super::StreamHook::on_finish(hooks, &self.cx, &status);
            }
        }
    }
}
impl Drop for OpenAttempt {
    fn drop(&mut self) {
        if self.success || self.cx.status().is_some() {
            return;
        }
        let error = self
            .cx
            .cancellation()
            .unwrap_or_else(|| StreamError::Canceled {
                cause: super::CancelCause::Drop,
                code: 12011,
                message: "stream open was abandoned".into(),
                path: Vec::new(),
            });
        self.cx.cancel(error.clone());
        self.fail(self.cx.cancellation().unwrap_or(error));
    }
}
/// Account only time queued behind another opener or pool capacity. The guard
/// also records a wait abandoned by timeout/cancellation; dial and HEADER work
/// never own this guard. The uncontended mutex path needs no clock read.
struct PoolWait<'a> {
    counter: &'a std::sync::atomic::AtomicU64,
    started: Instant,
}
impl<'a> PoolWait<'a> {
    fn new(cx: &'a StreamContext) -> Self {
        Self {
            counter: &cx.inner.resources.pool_wait_nanos,
            started: Instant::now(),
        }
    }
}
impl Drop for PoolWait<'_> {
    fn drop(&mut self) {
        self.counter.fetch_add(
            super::resources::nanos(self.started.elapsed()),
            Ordering::Relaxed,
        );
    }
}
impl<M: MakeTransport> TransportOpen<M> {
    async fn acquire(
        &self,
        cx: &mut StreamContext,
        request: OpenStream,
    ) -> Result<OpenedStream, StreamError> {
        if !cfg!(feature = "ttstream") {
            return Err(StreamError::configuration("ttstream feature is disabled"));
        }
        self.validation
            .get_or_init(|| self.config.validate())
            .as_ref()
            .map_err(Clone::clone)?;
        let address = match request
            .address
            .or_else(|| cx.with_rpc_info(|rpc| rpc.callee().address()))
        {
            Some(address) => address,
            None => {
                let resolver = self.resolver.as_ref().ok_or_else(|| {
                    StreamError::configuration(
                        "streaming target address is missing; a custom unary LB layer must \
                         provide a streaming resolver or layer",
                    )
                })?;
                let endpoint = EndpointLease::take(cx);
                resolver
                    .resolve(endpoint.value.as_ref().expect("leased endpoint"))
                    .await
                    .map_err(|error| {
                        StreamError::configuration(format!(
                            "streaming discovery/load balance: {error}"
                        ))
                    })?
            }
        };
        cx.update_rpc_info(|rpc| rpc.callee_mut().set_address(address.clone()));
        let cached = self.pool.read().get(&address).cloned();
        let target = if let Some(target) = cached {
            target
        } else {
            let mut pool = self.pool.write();
            if !pool.contains_key(&address) && pool.len() == pool.capacity() {
                pool.retain(|_, target| {
                    Arc::strong_count(target) > 1
                        || target.entries.lock().iter().any(|entry| {
                            !entry.connection.is_closed()
                                && (entry.leases.load(Ordering::Acquire) != 0
                                    || entry.idle.lock().elapsed() < self.config.pool_idle_timeout)
                        })
                });
                // A past endpoint spike must not set the next cleanup threshold
                // forever. Resize only at the existing lazy cleanup boundary.
                let retained = pool.len().max(64);
                pool.shrink_to(retained);
            }
            pool.entry(address.clone()).or_default().clone()
        };
        let acquire = async {
            let mut allow_expansion = true;
            loop {
                let notified = target.wake.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let (chosen, connecting, expanding) = {
                    let mut entries = target.entries.lock();
                    entries.retain(|entry| {
                        if entry.connection.is_closed() {
                            return false;
                        }
                        if entry.leases.load(Ordering::Acquire) == 0
                            && entry.idle.lock().elapsed() >= self.config.pool_idle_timeout
                        {
                            entry.connection.retire();
                            return false;
                        }
                        true
                    });
                    let chosen = entries
                        .iter()
                        .filter(|entry| {
                            let leases = entry.leases.load(Ordering::Acquire);
                            match self.config.pool_mode {
                                PoolMode::Short => false,
                                PoolMode::Long => leases == 0,
                                PoolMode::Multiplex => leases < self.config.max_streams,
                            }
                        })
                        .min_by_key(|entry| entry.leases.load(Ordering::Acquire));
                    let connecting = target.connecting.load(Ordering::Acquire);
                    // A soft target spreads long-lived multiplexed calls across
                    // I/O tasks well before the protocol's hard stream limit.
                    // Multiplex also serializes cold dials: waiters can share
                    // the first connection instead of opening one each.
                    let expand = chosen.is_some_and(|entry| {
                        allow_expansion
                            && self.config.pool_mode == PoolMode::Multiplex
                            && connecting == 0
                            && entry.leases.load(Ordering::Acquire)
                                >= 64.min(self.config.max_streams)
                    });
                    if (chosen.is_none() || expand)
                        && (self.config.pool_mode != PoolMode::Multiplex || connecting == 0)
                        && entries.len() + connecting < self.config.pool_max_active
                    {
                        target.connecting.fetch_add(1, Ordering::AcqRel);
                        (None, Some(Connecting(&target)), expand)
                    } else {
                        (
                            chosen.map(|entry| {
                                entry.leases.fetch_add(1, Ordering::AcqRel);
                                entry.clone()
                            }),
                            None,
                            false,
                        )
                    }
                };
                let entry = if let Some(chosen) = chosen {
                    chosen
                } else if let Some(connecting) = connecting {
                    // A connect timeout affects only this new dial. Never mutate
                    // the shared maker or an already-pooled connection.
                    let mut transport = self.transport.clone();
                    if let Some(timeout) = cx.with_rpc_info(|rpc| rpc.config().connect_timeout()) {
                        transport.set_connect_timeout(Some(timeout));
                    }
                    let dial = transport.make_transport(address.clone());
                    #[cfg(feature = "shmipc")]
                    let dial = volo::net::shmipc::config::SESSION_MANAGER_NAMESPACE
                        .scope("ttstream", dial);
                    let (reader, writer) = match dial.await {
                        Ok(transport) => transport,
                        Err(_) if expanding => {
                            // Optional expansion must not fail an RPC while a
                            // healthy multiplexed connection still has capacity.
                            drop(connecting);
                            allow_expansion = false;
                            continue;
                        }
                        Err(error) => {
                            return Err(StreamError::transport(error, IoPhase::Connect, false));
                        }
                    };
                    let connection = Connection::start(
                        reader,
                        writer,
                        self.config.clone(),
                        self.hooks.clone(),
                        None,
                    )?;
                    let entry = Arc::new(PoolEntry {
                        connection,
                        leases: AtomicUsize::new(1),
                        idle: Mutex::new(Instant::now()),
                    });
                    target.entries.lock().push(entry.clone());
                    drop(connecting);
                    entry
                } else {
                    let _waiting = PoolWait::new(cx);
                    notified.await;
                    continue;
                };
                let connection = entry.connection.clone();
                let lease = Lease {
                    entry,
                    target: target.clone(),
                    short: self.config.pool_mode == PoolMode::Short,
                    max_idle: self.config.pool_max_idle,
                };
                let result = connection
                    .open_prepared_context(cx.clone(), request.metadata.clone())
                    .await;
                return match result {
                    Ok(mut stream) => {
                        stream.set_lease(Box::new(lease));
                        Ok(stream)
                    }
                    Err(error) => {
                        drop(lease);
                        Err(error)
                    }
                };
            }
        };
        let result = tokio::time::timeout(self.config.pool_wait_timeout, acquire)
            .await
            .map_err(|_| {
                StreamError::configuration("streaming connection pool acquisition timed out")
            });
        result?
    }
}

#[cfg(all(test, feature = "ttstream"))]
mod tests {
    use std::{io, time::Duration};

    use bytes::Bytes;
    use volo::context::Role;

    use super::*;
    use crate::transport::ttstream::connection::{StreamRouter, next_id};

    #[derive(Clone, Default)]
    struct Dial {
        count: Arc<AtomicUsize>,
        block: bool,
        fail_after: Option<usize>,
        metadata: Arc<Mutex<Option<Metadata>>>,
    }
    impl MakeTransport for Dial {
        type ReadHalf = tokio::io::ReadHalf<tokio::io::DuplexStream>;
        type WriteHalf = tokio::io::WriteHalf<tokio::io::DuplexStream>;
        async fn make_transport(
            &self,
            _: Address,
        ) -> io::Result<(Self::ReadHalf, Self::WriteHalf)> {
            let index = self.count.fetch_add(1, Ordering::Relaxed);
            if self.fail_after.is_some_and(|limit| index >= limit) {
                return Err(io::Error::other("fixture dial failure"));
            }
            if self.block {
                return futures::future::pending().await;
            }
            let metadata = self.metadata.clone();
            let (client, server) = tokio::io::duplex(4096);
            let (r, w) = tokio::io::split(server);
            let mut router = StreamRouter::new();
            router
                .add("Echo", "chat", move |cx, mut stream| {
                    let metadata = metadata.clone();
                    async move {
                        *metadata.lock() = Some(cx.metadata());
                        while let Some(bytes) = stream.recv_raw().await? {
                            stream.send_raw(bytes).await?;
                        }
                        Ok(())
                    }
                })
                .unwrap();
            Connection::start(r, w, StreamConfig::default(), None, Some(router)).unwrap();
            Ok(tokio::io::split(client))
        }
        fn set_connect_timeout(&mut self, _: Option<Duration>) {}
        fn set_read_timeout(&mut self, _: Option<Duration>) {}
        fn set_write_timeout(&mut self, _: Option<Duration>) {}
    }
    async fn open(service: &OpenService) -> Result<OpenedStream, StreamError> {
        let mut cx = StreamContext::new(next_id(), "Echo", "chat", Role::Client);
        service
            .call(
                &mut cx,
                OpenStream {
                    address: Some(
                        "127.0.0.1:1"
                            .parse::<std::net::SocketAddr>()
                            .unwrap()
                            .into(),
                    ),
                    ..Default::default()
                },
            )
            .await
    }
    async fn finish(mut stream: OpenedStream) {
        stream
            .send_raw(Bytes::from_static(b"message"))
            .await
            .unwrap();
        stream.close_send().await.unwrap();
        assert_eq!(stream.recv_raw().await.unwrap().unwrap(), "message");
        assert!(stream.recv_raw().await.unwrap().is_none());
    }
    #[derive(Clone)]
    struct SlowDial {
        entered: Arc<Notify>,
        release: Arc<tokio::sync::Semaphore>,
        inner: Dial,
    }
    impl MakeTransport for SlowDial {
        type ReadHalf = <Dial as MakeTransport>::ReadHalf;
        type WriteHalf = <Dial as MakeTransport>::WriteHalf;
        async fn make_transport(
            &self,
            address: Address,
        ) -> io::Result<(Self::ReadHalf, Self::WriteHalf)> {
            self.entered.notify_one();
            self.release.acquire().await.unwrap().forget();
            self.inner.make_transport(address).await
        }
        fn set_connect_timeout(&mut self, _: Option<Duration>) {}
        fn set_read_timeout(&mut self, _: Option<Duration>) {}
        fn set_write_timeout(&mut self, _: Option<Duration>) {}
    }
    fn request() -> OpenStream {
        OpenStream {
            address: Some(
                "127.0.0.1:1"
                    .parse::<std::net::SocketAddr>()
                    .unwrap()
                    .into(),
            ),
            ..Default::default()
        }
    }
    #[tokio::test]
    async fn uncontended_slow_dial_is_not_pool_wait() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let service = OpenOptions {
            config: StreamConfig {
                pool_mode: PoolMode::Short,
                ..Default::default()
            },
            ..Default::default()
        }
        .build(
            SlowDial {
                entered: entered.clone(),
                release: release.clone(),
                inner: Dial::default(),
            },
            None,
        );
        let begin = Instant::now();
        let mut opening = Box::pin(open(&service));
        assert!(futures::poll!(opening.as_mut()).is_pending());
        entered.notified().await;
        tokio::time::sleep(Duration::from_millis(25)).await;
        release.add_permits(1);
        let stream = opening.await.unwrap();
        assert!(begin.elapsed() >= Duration::from_millis(25));
        assert_eq!(
            stream.context().resources().pool_wait_nanos,
            0,
            "own slow dial must not be charged to pool waiting"
        );
        finish(stream).await;
    }

    #[tokio::test]
    async fn multiplex_cold_burst_shares_one_inflight_dial() {
        for cancel_first in [false, true] {
            let entered = Arc::new(Notify::new());
            let release = Arc::new(tokio::sync::Semaphore::new(0));
            let dial = Dial::default();
            let service = OpenOptions {
                config: StreamConfig {
                    pool_mode: PoolMode::Multiplex,
                    ..Default::default()
                },
                ..Default::default()
            }
            .build(
                SlowDial {
                    entered: entered.clone(),
                    release: release.clone(),
                    inner: dial.clone(),
                },
                None,
            );
            let mut openings: Vec<_> = (0..32).map(|_| Box::pin(open(&service))).collect();
            assert!(futures::poll!(&mut openings[0]).is_pending());
            entered.notified().await;
            for opening in &mut openings[1..] {
                assert!(futures::poll!(opening.as_mut()).is_pending());
            }
            // No second make_transport may enter while the first handshake is held.
            assert!(futures::poll!(Box::pin(entered.notified())).is_pending());
            if cancel_first {
                drop(openings.remove(0));
                assert!(futures::poll!(&mut openings[0]).is_pending());
                assert!(futures::poll!(Box::pin(entered.notified())).is_ready());
            }
            release.add_permits(32);
            let streams = tokio::time::timeout(
                Duration::from_secs(2),
                futures::future::try_join_all(openings),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(dial.count.load(Ordering::Acquire), 1);
            for stream in streams {
                finish(stream).await;
            }
        }
    }
    #[tokio::test]
    async fn independent_dials_do_not_hold_the_endpoint_lock() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let service = OpenOptions {
            config: StreamConfig {
                pool_mode: PoolMode::Short,
                ..Default::default()
            },
            ..Default::default()
        }
        .build(
            SlowDial {
                entered: entered.clone(),
                release: release.clone(),
                inner: Dial::default(),
            },
            None,
        );
        let mut first_cx = StreamContext::new(next_id(), "Echo", "chat", Role::Client);
        let first = service.call(&mut first_cx, request());
        tokio::pin!(first);
        let begin = Instant::now();
        assert!(futures::poll!(first.as_mut()).is_pending());
        entered.notified().await;
        let mut waiting_cx = StreamContext::new(next_id(), "Echo", "chat", Role::Client);
        let observation = waiting_cx.clone();
        let mut second = Box::pin(service.call(&mut waiting_cx, request()));
        assert!(futures::poll!(second.as_mut()).is_pending());
        tokio::time::timeout(Duration::from_millis(100), entered.notified())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;
        drop(second);
        assert_eq!(observation.resources().pool_wait_nanos, 0);
        release.add_permits(1);
        let first = first.await.unwrap();
        assert!(begin.elapsed() >= Duration::from_millis(25));
        assert_eq!(
            first.context().resources().pool_wait_nanos,
            0,
            "own slow dial is not pool contention"
        );
        finish(first).await;
    }
    #[tokio::test]
    async fn capacity_wait_is_counted_on_cancellation_and_acquire_timeout() {
        let service = OpenOptions {
            config: StreamConfig {
                pool_max_active: 1,
                pool_wait_timeout: Duration::from_millis(100),
                ..Default::default()
            },
            ..Default::default()
        }
        .build(Dial::default(), None);
        let first = open(&service).await.unwrap();
        let mut cx = StreamContext::new(next_id(), "Echo", "chat", Role::Client);
        let observation = cx.clone();
        let mut waiting = Box::pin(service.call(&mut cx, request()));
        assert!(futures::poll!(waiting.as_mut()).is_pending());
        tokio::time::sleep(Duration::from_millis(25)).await;
        drop(waiting);
        assert!(observation.resources().pool_wait_nanos >= 25_000_000);
        let mut timed = StreamContext::new(next_id(), "Echo", "chat", Role::Client);
        let result = service.call(&mut timed, request()).await;
        assert!(result.is_err());
        assert!(timed.resources().pool_wait_nanos >= 100_000_000);
        finish(first).await;
    }
    #[tokio::test]
    async fn multiplex_expands_before_hard_limit_balances_and_tolerates_failed_expansion() {
        for fail_after in [None, Some(1)] {
            let dial = Dial {
                fail_after,
                ..Default::default()
            };
            let pools = Arc::new(RwLock::default());
            let service = OpenService::new(TransportOpen {
                transport: dial,
                config: StreamConfig {
                    pool_mode: PoolMode::Multiplex,
                    pool_max_active: 2,
                    ..Default::default()
                },
                validation: Default::default(),
                hooks: None,
                pool: pools.clone(),
                resolver: None,
            });
            let mut calls = Vec::new();
            for _ in 0..96 {
                calls.push(open(&service).await.unwrap());
            }
            let target = pools.read().values().next().unwrap().clone();
            let loads: Vec<_> = target
                .entries
                .lock()
                .iter()
                .map(|entry| entry.leases.load(Ordering::Acquire))
                .collect();
            assert_eq!(
                loads,
                if fail_after.is_none() {
                    vec![64, 32]
                } else {
                    vec![96]
                }
            );
            for call in calls {
                finish(call).await;
            }
        }
    }

    #[tokio::test]
    async fn pools_reuse_and_short_capacity_is_bounded() {
        for mode in [PoolMode::Short, PoolMode::Long, PoolMode::Multiplex] {
            let dial = Dial::default();
            let config = StreamConfig {
                pool_mode: mode,
                pool_max_active: 1,
                max_streams: 1,
                pool_wait_timeout: Duration::from_millis(25),
                ..Default::default()
            };
            let service = OpenOptions {
                config,
                ..Default::default()
            }
            .build(dial.clone(), None);
            let first = open(&service).await.unwrap();
            assert!(
                open(&service).await.is_err(),
                "pool mode {mode:?} must enforce capacity"
            );
            finish(first).await;
            finish(open(&service).await.unwrap()).await;
            assert_eq!(
                dial.count.load(Ordering::Relaxed),
                if mode == PoolMode::Short { 2 } else { 1 }
            );
        }
    }
    #[tokio::test]
    async fn long_pool_cancel_flush_overlap_expands_then_retires_idle_tail() {
        use std::{
            sync::atomic::AtomicBool,
            task::{Context, Poll},
        };

        use tokio::io::AsyncWrite;

        #[derive(Default)]
        struct Gate {
            blocked: AtomicBool,
            entered: Notify,
            waker: futures::task::AtomicWaker,
        }
        struct Writer {
            inner: <Dial as MakeTransport>::WriteHalf,
            gate: Option<Arc<Gate>>,
        }
        impl AsyncWrite for Writer {
            fn poll_write(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                bytes: &[u8],
            ) -> Poll<io::Result<usize>> {
                Pin::new(&mut self.inner).poll_write(cx, bytes)
            }
            fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                if let Some(gate) = &self.gate {
                    gate.waker.register(cx.waker());
                    if gate.blocked.load(Ordering::Acquire) {
                        gate.entered.notify_one();
                        return Poll::Pending;
                    }
                }
                Pin::new(&mut self.inner).poll_flush(cx)
            }
            fn poll_shutdown(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<io::Result<()>> {
                Pin::new(&mut self.inner).poll_shutdown(cx)
            }
        }
        #[derive(Clone)]
        struct GatedDial {
            dial: Dial,
            gate: Arc<Gate>,
        }
        impl MakeTransport for GatedDial {
            type ReadHalf = <Dial as MakeTransport>::ReadHalf;
            type WriteHalf = Writer;
            async fn make_transport(
                &self,
                addr: Address,
            ) -> io::Result<(Self::ReadHalf, Self::WriteHalf)> {
                let (read, inner) = self.dial.make_transport(addr).await?;
                let gate =
                    (self.dial.count.load(Ordering::Acquire) == 1).then(|| self.gate.clone());
                Ok((read, Writer { inner, gate }))
            }
            fn set_connect_timeout(&mut self, _: Option<Duration>) {}
            fn set_read_timeout(&mut self, _: Option<Duration>) {}
            fn set_write_timeout(&mut self, _: Option<Duration>) {}
        }
        let dial = Dial::default();
        let gate = Arc::new(Gate::default());
        let pools = Arc::new(RwLock::new(FxHashMap::default()));
        let service = OpenService::new(TransportOpen {
            transport: GatedDial {
                dial: dial.clone(),
                gate: gate.clone(),
            },
            config: StreamConfig {
                pool_idle_timeout: Duration::from_millis(40),
                ..Default::default()
            },
            hooks: None,
            pool: pools.clone(),
            validation: Default::default(),
            resolver: None,
        });
        let mut first = open(&service).await.unwrap();
        first.send_raw(Bytes::from_static(b"ready")).await.unwrap();
        assert_eq!(first.recv_raw().await.unwrap().unwrap(), "ready");
        gate.blocked.store(true, Ordering::Release);
        first.context().cancel_local("pool overlap");
        assert_eq!(
            first.recv_raw().await.unwrap_err().kitex_code(),
            Some(12007)
        );
        tokio::time::timeout(Duration::from_secs(1), gate.entered.notified())
            .await
            .unwrap();
        let target = pools.read().values().next().unwrap().clone();
        assert_eq!(target.entries.lock()[0].leases.load(Ordering::Acquire), 1);
        finish(open(&service).await.unwrap()).await;
        assert_eq!(
            dial.count.load(Ordering::Acquire),
            2,
            "pending RST flush retains the first lease"
        );
        gate.blocked.store(false, Ordering::Release);
        gate.waker.wake();
        tokio::time::timeout(Duration::from_secs(1), async {
            while target.entries.lock()[0].leases.load(Ordering::Acquire) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let reused = open(&service).await.unwrap();
        assert_eq!(dial.count.load(Ordering::Acquire), 2);
        let tail = target.entries.lock()[1].connection.clone();
        tokio::time::timeout(Duration::from_secs(1), tail.closed())
            .await
            .unwrap();
        assert!(
            !target.entries.lock()[0].connection.is_closed(),
            "active reused connection remains alive"
        );
        finish(reused).await;
    }

    #[tokio::test]
    async fn zero_idle_budget_and_idle_timeout_retire_connections() {
        for max_idle in [0, 1] {
            let dial = Dial::default();
            let config = StreamConfig {
                pool_max_idle: max_idle,
                pool_idle_timeout: Duration::from_millis(25),
                ..Default::default()
            };
            let service = OpenOptions {
                config,
                ..Default::default()
            }
            .build(dial.clone(), None);
            finish(open(&service).await.unwrap()).await;
            if max_idle != 0 {
                tokio::time::sleep(Duration::from_millis(60)).await;
            }
            finish(open(&service).await.unwrap()).await;
            assert_eq!(dial.count.load(Ordering::Relaxed), 2);
        }
    }
    #[tokio::test]
    async fn opened_stream_owns_pool_until_terminal_after_client_drop() {
        let service = OpenOptions::default().build(Dial::default(), None);
        let stream = open(&service).await.unwrap();
        drop(service);
        finish(stream).await;
    }
    #[derive(Default)]
    struct OpenHook {
        opens: AtomicUsize,
        finishes: AtomicUsize,
        errors: AtomicUsize,
    }
    impl super::super::StreamHook for OpenHook {
        fn on_open(&self, cx: &mut StreamContext) -> Result<(), StreamError> {
            use metainfo::Forward;
            self.opens.fetch_add(1, Ordering::Relaxed);
            cx.update_metadata(|metadata| {
                metadata.string.remove(&Bytes::from_static(b"removed"));
                metadata.int.remove(&778);
                metadata.int.insert(777, Bytes::from_static(b"caller"));
                metadata.insert(
                    Bytes::from_static(b"opaque"),
                    Bytes::from_static(b"hook\xff"),
                );
            });
            cx.sync_scope(|| {
                metainfo::METAINFO.with(|info| info.borrow_mut().set_persistent("span", "injected"))
            });
            Ok(())
        }
        fn on_io_error(&self, _: &StreamContext, _: &super::super::StreamIoError) {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
        fn on_finish(&self, _: &StreamContext, _: &super::super::StreamStatus) {
            self.finishes.fetch_add(1, Ordering::Relaxed);
        }
    }
    #[tokio::test]
    async fn open_hook_metadata_is_exported_after_hook_and_finishes_once() {
        let hook = Arc::new(OpenHook::default());
        let dial = Dial::default();
        let service = OpenOptions {
            hooks: Some(HookChain::new([
                hook.clone() as Arc<dyn super::super::StreamHook>
            ])),
            ..Default::default()
        }
        .build(dial.clone(), None);
        let mut cx = StreamContext::new(next_id(), "Echo", "chat", Role::Client);
        let mut metadata = Metadata::default();
        metadata.int.insert(777, Bytes::from_static(b"request"));
        metadata.insert(
            Bytes::from_static(b"opaque"),
            Bytes::from_static(b"request"),
        );
        metadata.insert(
            Bytes::from_static(b"untouched"),
            Bytes::from_static(b"preserved"),
        );
        metadata.insert(
            Bytes::from_static(b"removed"),
            Bytes::from_static(b"must not reappear"),
        );
        metadata
            .int
            .insert(778, Bytes::from_static(b"must not reappear"));
        let stream = service
            .call(
                &mut cx,
                OpenStream {
                    address: Some(
                        "127.0.0.1:1"
                            .parse::<std::net::SocketAddr>()
                            .unwrap()
                            .into(),
                    ),
                    metadata,
                },
            )
            .await
            .unwrap();
        finish(stream).await;
        assert_eq!(
            dial.metadata
                .lock()
                .as_ref()
                .unwrap()
                .get("untouched")
                .unwrap(),
            "preserved"
        );
        assert_eq!(
            dial.metadata
                .lock()
                .as_ref()
                .unwrap()
                .get("RPC_PERSIST_span")
                .unwrap(),
            "injected"
        );
        assert_eq!(
            dial.metadata
                .lock()
                .as_ref()
                .unwrap()
                .int
                .get(&777)
                .unwrap(),
            "caller"
        );
        assert_eq!(
            dial.metadata
                .lock()
                .as_ref()
                .unwrap()
                .get("opaque")
                .unwrap(),
            &Bytes::from_static(b"hook\xff")
        );
        assert!(
            dial.metadata
                .lock()
                .as_ref()
                .unwrap()
                .get("removed")
                .is_none()
        );
        assert!(
            !dial
                .metadata
                .lock()
                .as_ref()
                .unwrap()
                .int
                .contains_key(&778)
        );
        assert_eq!(hook.opens.load(Ordering::Relaxed), 1);
        assert_eq!(hook.finishes.load(Ordering::Relaxed), 1);
    }
    #[tokio::test]
    async fn canceled_open_and_local_deadline_finish_before_any_header() {
        for deadline in [None, Some(Duration::from_millis(5))] {
            let hook = Arc::new(OpenHook::default());
            let dial = Dial {
                block: true,
                ..Default::default()
            };
            let service = OpenOptions {
                config: StreamConfig {
                    deadline,
                    ..Default::default()
                },
                hooks: Some(HookChain::new([
                    hook.clone() as Arc<dyn super::super::StreamHook>
                ])),
                ..Default::default()
            }
            .build(dial.clone(), None);
            let mut cx = StreamContext::new(next_id(), "Echo", "chat", Role::Client);
            let request = OpenStream {
                address: Some(
                    "127.0.0.1:1"
                        .parse::<std::net::SocketAddr>()
                        .unwrap()
                        .into(),
                ),
                ..Default::default()
            };
            let result =
                tokio::time::timeout(Duration::from_millis(25), service.call(&mut cx, request))
                    .await;
            if deadline.is_some() {
                assert!(matches!(
                    result,
                    Ok(Err(StreamError::Canceled { code: 12015, .. }))
                ));
            } else {
                assert!(result.is_err());
            }
            assert_eq!(dial.count.load(Ordering::Relaxed), 1);
            assert_eq!(hook.opens.load(Ordering::Relaxed), 1);
            assert_eq!(hook.finishes.load(Ordering::Relaxed), 1);
            assert!(cx.status().unwrap().error.is_some());
        }
    }
    #[tokio::test]
    async fn configured_discovery_is_used_without_an_explicit_stream_address() {
        use volo::{
            discovery::StaticDiscover,
            loadbalance::{LbConfig, MkLbLayer, random::WeightedRandomBalance},
        };
        let address = "127.0.0.1:4321".parse::<std::net::SocketAddr>().unwrap();
        let (_, resolver) = LbConfig::new(
            WeightedRandomBalance::new(),
            StaticDiscover::from(vec![address]),
        )
        .make_with_resolver();
        let service = OpenOptions::default().build(Dial::default(), resolver);
        let mut cx = StreamContext::new(next_id(), "Echo", "chat", Role::Client);
        let stream = service.call(&mut cx, OpenStream::default()).await.unwrap();
        assert_eq!(
            cx.with_rpc_info(|rpc| rpc.callee().address()),
            Some(address.into())
        );
        finish(stream).await;
    }
    #[derive(Clone, Copy)]
    struct FailLayer {
        pending: bool,
    }
    impl Layer<OpenService> for FailLayer {
        type Service = FailOpen;
        fn layer(self, _: OpenService) -> Self::Service {
            FailOpen {
                pending: self.pending,
            }
        }
    }
    struct FailOpen {
        pending: bool,
    }
    impl Service<StreamContext, OpenStream> for FailOpen {
        type Response = OpenedStream;
        type Error = StreamError;
        async fn call(
            &self,
            _: &mut StreamContext,
            _: OpenStream,
        ) -> Result<OpenedStream, StreamError> {
            if self.pending {
                std::future::pending().await
            } else {
                Err(StreamError::configuration("layer failure before transport"))
            }
        }
    }
    #[tokio::test]
    async fn front_open_layer_wraps_registered_layers_and_observes_outer_error_and_drop() {
        #[derive(Clone)]
        struct TraceLayer {
            enter: &'static str,
            exit: &'static str,
            events: Arc<Mutex<Vec<&'static str>>>,
        }
        struct TraceService {
            inner: OpenService,
            trace: TraceLayer,
        }
        impl Layer<OpenService> for TraceLayer {
            type Service = TraceService;
            fn layer(self, inner: OpenService) -> Self::Service {
                TraceService { inner, trace: self }
            }
        }
        struct Exit(TraceLayer);
        impl Drop for Exit {
            fn drop(&mut self) {
                self.0.events.lock().push(self.0.exit);
            }
        }
        impl Service<StreamContext, OpenStream> for TraceService {
            type Response = OpenedStream;
            type Error = StreamError;
            async fn call(
                &self,
                cx: &mut StreamContext,
                request: OpenStream,
            ) -> Result<OpenedStream, StreamError> {
                self.trace.events.lock().push(self.trace.enter);
                let _exit = Exit(self.trace.clone());
                self.inner.call(cx, request).await
            }
        }
        for pending in [false, true] {
            let events = Arc::new(Mutex::new(Vec::new()));
            let trace = |enter, exit| TraceLayer {
                enter,
                exit,
                events: events.clone(),
            };
            let mut options = OpenOptions {
                service: Some(OpenService::new(FailOpen { pending })),
                ..Default::default()
            };
            options.layer(trace("a-enter", "a-exit"));
            options.layer(trace("b-enter", "b-exit"));
            options.layer_front(trace("front-enter", "front-exit"));
            let service = options.build(Dial::default(), None);
            let result = tokio::time::timeout(Duration::from_millis(5), open(&service)).await;
            if pending {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            assert_eq!(
                *events.lock(),
                [
                    "front-enter",
                    "a-enter",
                    "b-enter",
                    "b-exit",
                    "a-exit",
                    "front-exit"
                ]
            );

            events.lock().clear();
            let mut options = OpenOptions::default();
            options.layer(FailLayer { pending });
            options.layer_front(trace("front-enter", "front-exit"));
            let dial = Dial::default();
            let service = options.build(dial.clone(), None);
            let result = tokio::time::timeout(Duration::from_millis(5), open(&service)).await;
            if pending {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            assert_eq!(*events.lock(), ["front-enter", "front-exit"]);
            assert_eq!(dial.count.load(Ordering::Acquire), 0);
        }
    }

    #[tokio::test]
    async fn outer_open_boundary_observes_layer_custom_service_deadline_and_drop() {
        for mode in 0..4 {
            let hook = Arc::new(OpenHook::default());
            let mut options = OpenOptions {
                hooks: Some(HookChain::new([
                    hook.clone() as Arc<dyn super::super::StreamHook>
                ])),
                config: StreamConfig {
                    deadline: (mode == 2).then_some(Duration::from_millis(5)),
                    ..Default::default()
                },
                ..Default::default()
            };
            if mode == 1 {
                options.service = Some(OpenService::new(FailOpen { pending: false }));
            } else {
                options.layer(FailLayer { pending: mode >= 2 });
            }
            let service = options.build(Dial::default(), None);
            let mut cx = StreamContext::new(next_id(), "Echo", "chat", Role::Client);
            let result = tokio::time::timeout(
                Duration::from_millis(20),
                service.call(&mut cx, OpenStream::default()),
            )
            .await;
            if mode == 3 {
                assert!(result.is_err());
            } else if mode == 2 {
                assert!(matches!(
                    result,
                    Ok(Err(StreamError::Canceled { code: 12015, .. }))
                ));
            } else {
                assert!(matches!(result, Ok(Err(StreamError::Configuration(_)))));
            }
            assert_eq!(hook.opens.load(Ordering::Relaxed), usize::from(mode == 1));
            assert_eq!(hook.finishes.load(Ordering::Relaxed), 1);
            assert_eq!(hook.errors.load(Ordering::Relaxed), 1);
            assert!(cx.status().unwrap().error.is_some());
            assert!(
                cx.started_at().elapsed()
                    >= if mode >= 2 {
                        Duration::from_millis(4)
                    } else {
                        Duration::ZERO
                    }
            );
        }
    }
}

#[cfg(all(test, not(feature = "ttstream")))]
mod disabled_tests {
    use super::*;
    use crate::streaming::{StreamHook, StreamIoError, StreamStatus};
    struct Unreachable(Arc<AtomicUsize>);
    impl Service<StreamContext, OpenStream> for Unreachable {
        type Response = OpenedStream;
        type Error = StreamError;
        async fn call(
            &self,
            _: &mut StreamContext,
            _: OpenStream,
        ) -> Result<OpenedStream, StreamError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            panic!("a disabled feature must not execute a layer or custom service")
        }
    }
    #[derive(Default)]
    struct Events {
        errors: AtomicUsize,
        finishes: AtomicUsize,
    }
    impl StreamHook for Events {
        fn on_io_error(&self, _: &StreamContext, event: &StreamIoError) {
            assert!(matches!(event.error, StreamError::Configuration(_)));
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
        fn on_finish(&self, _: &StreamContext, _: &StreamStatus) {
            self.finishes.fetch_add(1, Ordering::Relaxed);
        }
    }
    #[tokio::test]
    async fn disabled_feature_rejects_before_outer_layers_and_custom_services() {
        let calls = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(Events::default());
        let mut hooks = HookChain::default();
        hooks.push(events.clone());
        let service = OpenBoundary {
            inner: OpenService::new(Unreachable(calls.clone())),
            hooks: Some(hooks),
            deadline: None,
        };
        let mut cx = StreamContext::new(1, "Test", "Bidi", volo::context::Role::Client);
        let error = service
            .call(&mut cx, OpenStream::default())
            .await
            .err()
            .unwrap();
        assert!(matches!(error, StreamError::Configuration(_)));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(events.errors.load(Ordering::Relaxed), 1);
        assert_eq!(events.finishes.load(Ordering::Relaxed), 1);
    }
}
