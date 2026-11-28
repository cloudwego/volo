use std::{
    future::Future,
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{sync::watch, task::JoinSet};
use volo::{
    Layer, Service,
    layer::{Identity, Stack},
    net::{
        conn::{BufferedConnParts, Conn, OwnedReadHalf, OwnedWriteHalf, PeekedConn},
        incoming::{Incoming, MakeIncoming},
    },
    streaming::ServerDrain,
};
use volo_grpc::{Status, body::BoxBody};
use volo_thrift::{
    codec::{
        MakeCodec,
        default::{
            DefaultMakeCodec, framed::MakeFramedCodec, thrift::MakeThriftCodec,
            ttheader::MakeTTHeaderCodec,
        },
    },
    server::{
        DetectedThrift,
        routed::{RoutedMakeCodec, RoutedRequest, RoutedResponse, RoutedService},
    },
};

use crate::{Binding, BoxError, FrozenRegistry, Registry};

type DefaultThriftCodec = DefaultMakeCodec<MakeTTHeaderCodec<MakeFramedCodec<MakeThriftCodec>>>;
pub type ThriftServer<
    L = Identity,
    C = DefaultThriftCodec,
    P = volo_thrift::tracing::DefaultProvider,
> = volo_thrift::server::Server<RoutedService, L, RoutedRequest, C, P>;
pub type GrpcServer<
    I = Identity,
    O = tower::layer::util::Identity,
    P = volo_grpc::tracing::DefaultProvider,
> = volo_grpc::server::Server<I, O, P>;
type ThriftLayerService = volo::service::BoxService<
    volo_thrift::context::ServerContext,
    RoutedRequest,
    RoutedResponse,
    volo_thrift::ServerError,
>;
type GrpcLayerService =
    tower::util::BoxCloneService<http::Request<BoxBody>, http::Response<BoxBody>, Status>;
type NativePrepared<S, C, P, G> =
    PreparedServer<NativeThrift<S, C, P>, volo_grpc::server::PreparedServer<G>>;
type NativeThrift<S, C, P> =
    volo_thrift::server::PreparedServer<S, RoutedRequest, RoutedMakeCodec<C>, P>;
type ThriftDriver<S, C, P> =
    volo_thrift::server::ConnectionDriver<S, RoutedRequest, RoutedMakeCodec<C>, P>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Thrift,
    Ttstream,
    Grpc,
}

/// An explicit subset of the registered services' transport capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Protocols(u8);
impl Protocols {
    pub const NONE: Self = Self(0);
    pub const THRIFT: Self = Self(1);
    pub const TTSTREAM: Self = Self(2);
    pub const GRPC: Self = Self(4);
    pub const ALL: Self = Self(7);
    pub fn contains(self, protocol: Protocol) -> bool {
        self.0 & Self::of(protocol).0 != 0
    }
    fn of(protocol: Protocol) -> Self {
        match protocol {
            Protocol::Thrift => Self::THRIFT,
            Protocol::Ttstream => Self::TTSTREAM,
            Protocol::Grpc => Self::GRPC,
        }
    }
    fn single(self) -> Option<Protocol> {
        if self.0.count_ones() != 1 {
            return None;
        }
        [Protocol::Thrift, Protocol::Ttstream, Protocol::Grpc]
            .into_iter()
            .find(|protocol| self.contains(*protocol))
    }
}
impl Default for Protocols {
    fn default() -> Self {
        Self::ALL
    }
}
impl std::ops::BitAnd for Protocols {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}
impl Protocols {
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}
impl std::ops::BitOr for Protocols {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

pub use volo::net::conn::ConnTransport as ConnectionTransport;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    Empty,
    ShortPrefix,
    Io,
    TlsHandshake,
    Timeout,
    TlsTransport,
    Alpn,
    DisabledProtocol,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionEvent {
    Selected {
        protocol: Protocol,
        transport: ConnectionTransport,
        detected: bool,
    },
    Rejected(Rejection),
    Closed(Option<Protocol>),
}
pub type ConnectionObserver = Arc<dyn Fn(ConnectionEvent) + Send + Sync>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConnectionStats {
    pub accepted: usize,
    pub active: usize,
    pub detected: usize,
    pub thrift: usize,
    pub ttstream: usize,
    pub grpc: usize,
    pub rejected: usize,
    pub driver_errors: usize,
    pub active_calls: usize,
    pub active_tasks: usize,
}

struct State {
    stop: watch::Sender<bool>,
    drain: ServerDrain,
    middleware: Option<volo::streaming::ServerMiddleware>,
    observer: Option<ConnectionObserver>,
    connection_timeout: Duration,
    accepted: AtomicUsize,
    active: AtomicUsize,
    detected: AtomicUsize,
    thrift: AtomicUsize,
    ttstream: AtomicUsize,
    grpc: AtomicUsize,
    rejected: AtomicUsize,
    driver_errors: AtomicUsize,
}
impl State {
    fn new(
        observer: Option<ConnectionObserver>,
        connection_timeout: Duration,
        middleware: Option<volo::streaming::ServerMiddleware>,
    ) -> Self {
        Self {
            stop: watch::channel(false).0,
            drain: ServerDrain::default(),
            middleware,
            observer,
            connection_timeout,
            accepted: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            detected: AtomicUsize::new(0),
            thrift: AtomicUsize::new(0),
            ttstream: AtomicUsize::new(0),
            grpc: AtomicUsize::new(0),
            rejected: AtomicUsize::new(0),
            driver_errors: AtomicUsize::new(0),
        }
    }
    fn event(&self, event: ConnectionEvent) {
        if let Some(observer) = &self.observer {
            // Closed is emitted from Drop, which may already be unwinding.
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer(event))).is_err() {
                tracing::error!("StreamX connection observer panicked");
            }
        }
    }
    fn reject(&self, reason: Rejection) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
        self.event(ConnectionEvent::Rejected(reason));
    }
}

#[derive(Clone)]
pub struct ServerHandle(Arc<State>);
impl ServerHandle {
    pub fn shutdown(&self) {
        self.0.stop.send_replace(true);
    }
    pub fn stats(&self) -> ConnectionStats {
        let state = &self.0;
        ConnectionStats {
            accepted: state.accepted.load(Ordering::Relaxed),
            active: state.active.load(Ordering::Relaxed),
            detected: state.detected.load(Ordering::Relaxed),
            thrift: state.thrift.load(Ordering::Relaxed),
            ttstream: state.ttstream.load(Ordering::Relaxed),
            grpc: state.grpc.load(Ordering::Relaxed),
            rejected: state.rejected.load(Ordering::Relaxed),
            driver_errors: state.driver_errors.load(Ordering::Relaxed),
            active_calls: state.drain.active_calls(),
            active_tasks: state.drain.active_tasks(),
        }
    }
}

/// One service directory, one incoming, and native connection drivers.
pub struct Server<
    TL = Identity,
    C = DefaultThriftCodec,
    TP = volo_thrift::tracing::DefaultProvider,
    GI = Identity,
    GO = tower::layer::util::Identity,
    GP = volo_grpc::tracing::DefaultProvider,
    L = Identity,
> {
    registry: Registry,
    error: Option<BoxError>,
    fallback: bool,
    protocols: Option<Protocols>,
    thrift: ThriftServer<TL, C, TP>,
    grpc: GrpcServer<GI, GO, GP>,
    observer: Option<ConnectionObserver>,
    connection_timeout: Duration,
    layer: L,
    has_layers: bool,
    hooks: Vec<Arc<dyn volo::streaming::StreamHookFactory>>,
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}
impl Server {
    pub fn new() -> Self {
        Self {
            registry: Registry::new(),
            error: None,
            fallback: false,
            protocols: None,
            thrift: ThriftServer::new(RoutedService).unsafe_codec(false),
            grpc: GrpcServer::new(),
            observer: None,
            connection_timeout: Duration::from_secs(10),
            layer: Identity::new(),
            has_layers: false,
            hooks: Vec::new(),
        }
    }
}

impl<TL, C, TP, GI, GO, GP, L> Server<TL, C, TP, GI, GO, GP, L> {
    /// Select the optional optimized Thrift codecs across unary, TTStream and gRPC.
    /// Off by default; requires `unsafe-codec`. Protobuf is unchanged.
    pub fn unsafe_codec(mut self, enabled: bool) -> Self {
        self.thrift = self.thrift.unsafe_codec(enabled);
        self.grpc = self.grpc.unsafe_codec(enabled);
        self
    }

    /// Observe native stream events and the native owner's one terminal status.
    pub fn stream_hook(mut self, factory: impl volo::streaming::StreamHookFactory) -> Self {
        self.hooks.push(Arc::new(factory));
        self
    }
    /// Install a framework hook before application hooks.
    #[doc(hidden)]
    pub fn stream_hook_front(mut self, factory: impl volo::streaming::StreamHookFactory) -> Self {
        self.hooks.insert(0, Arc::new(factory));
        self
    }
    /// Wrap each typed handler invocation, for every enabled protocol and RPC shape.
    /// Registration order is outermost first; messages do not re-enter this layer.
    pub fn layer<Inner>(self, layer: Inner) -> Server<TL, C, TP, GI, GO, GP, Stack<Inner, L>> {
        Server {
            registry: self.registry,
            error: self.error,
            fallback: self.fallback,
            protocols: self.protocols,
            thrift: self.thrift,
            grpc: self.grpc,
            observer: self.observer,
            connection_timeout: self.connection_timeout,
            layer: Stack::new(layer, self.layer),
            has_layers: true,
            hooks: self.hooks,
        }
    }
    pub fn add_service(self, binding: impl Binding) -> Self {
        self.add_registration(binding.registration())
    }
    /// Register a prepared typed binding in the same service directory.
    #[doc(hidden)]
    pub fn add_registration(mut self, registration: crate::ServiceRegistration) -> Self {
        if let Err(error) = self.registry.add(registration) {
            self.error.get_or_insert_with(|| Box::new(error));
        }
        self
    }

    /// Kitex's service-combination routing, independent of incoming/listener composition.
    pub fn combine_service(mut self, service: &str) -> Self {
        if let Err(error) = self.registry.combine_service(service) {
            self.error.get_or_insert_with(|| Box::new(error));
        }
        self
    }

    pub fn add_fallback_service(self, binding: impl Binding) -> Self {
        self.add_fallback_registration(binding.registration())
    }
    #[doc(hidden)]
    pub fn add_fallback_registration(mut self, registration: crate::ServiceRegistration) -> Self {
        if self.fallback {
            self.error.get_or_insert_with(|| {
                invalid("only one fallback service may be registered").into()
            });
            return self;
        }
        let name = registration.name();
        match self.registry.add(registration) {
            Ok(()) => {
                self.registry.fallback(name);
                self.fallback = true;
            }
            Err(error) => {
                self.error.get_or_insert_with(|| Box::new(error));
            }
        }
        self
    }
    pub fn refuse_traffic_without_service_name(mut self, refuse: bool) -> Self {
        self.registry.refuse_traffic_without_service_name(refuse);
        self
    }
    pub fn protocols(mut self, protocols: Protocols) -> Self {
        self.protocols = Some(protocols);
        self
    }
    pub fn connection_observer(
        mut self,
        observer: impl Fn(ConnectionEvent) + Send + Sync + 'static,
    ) -> Self {
        self.observer = Some(Arc::new(observer));
        self
    }
    /// Tune the native Thrift codec, layers, hooks and stream limits before preparation.
    pub fn thrift<NL, NC, NP>(
        self,
        configure: impl FnOnce(ThriftServer<TL, C, TP>) -> ThriftServer<NL, NC, NP>,
    ) -> Server<NL, NC, NP, GI, GO, GP, L> {
        Server {
            registry: self.registry,
            error: self.error,
            fallback: self.fallback,
            protocols: self.protocols,
            thrift: configure(self.thrift),
            grpc: self.grpc,
            observer: self.observer,
            connection_timeout: self.connection_timeout,
            layer: self.layer,
            has_layers: self.has_layers,
            hooks: self.hooks,
        }
    }
    /// Tune the native H2 layers, windows, keepalive and TLS settings.
    /// Service registration belongs to add_service/add_fallback_service.
    pub fn grpc<NI, NO, NP>(
        self,
        configure: impl FnOnce(GrpcServer<GI, GO, GP>) -> GrpcServer<NI, NO, NP>,
    ) -> Server<TL, C, TP, NI, NO, NP, L> {
        Server {
            registry: self.registry,
            error: self.error,
            fallback: self.fallback,
            protocols: self.protocols,
            thrift: self.thrift,
            grpc: configure(self.grpc),
            observer: self.observer,
            connection_timeout: self.connection_timeout,
            layer: self.layer,
            has_layers: self.has_layers,
            hooks: self.hooks,
        }
    }
    /// A single deadline applies to every transport, including pending detection and hooks.
    pub fn graceful_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.grpc = self.grpc.graceful_shutdown_timeout(timeout);
        self
    }
    /// Maximum total time for TLS negotiation and protocol detection.
    pub fn connection_timeout(mut self, timeout: Duration) -> Self {
        self.connection_timeout = timeout;
        self
    }
    #[cfg(feature = "__tls")]
    pub fn tls_config(mut self, config: impl Into<volo::net::tls::ServerTlsConfig>) -> Self {
        self.grpc = self.grpc.tls_config(config);
        self
    }
}

/// Fully validated service/layer state; no listener has been created.
pub struct PreparedServer<T, G> {
    thrift: T,
    grpc: G,
    directory: Arc<FrozenRegistry>,
    protocols: Protocols,
    state: Arc<State>,
}
impl<T, G> PreparedServer<T, G> {
    pub fn handle(&self) -> ServerHandle {
        ServerHandle(self.state.clone())
    }
    pub fn directory(&self) -> &Arc<FrozenRegistry> {
        &self.directory
    }
    pub fn protocols(&self) -> Protocols {
        self.protocols
    }
}

impl<TL, C, TP, GI, GO, GP, L> Server<TL, C, TP, GI, GO, GP, L>
where
    L: Layer<volo::streaming::InvocationHandler>,
    L::Service: for<'a> Service<
            volo::streaming::StreamContext,
            volo::streaming::Invocation<'a>,
            Response = (),
            Error = volo::streaming::RpcError,
        > + Send
        + Sync
        + 'static,
    TL: Layer<ThriftLayerService>,
    TL::Service: Service<
            volo_thrift::context::ServerContext,
            RoutedRequest,
            Response = RoutedResponse,
            Error = volo_thrift::ServerError,
        > + Send
        + Sync
        + 'static,
    C: MakeCodec<OwnedReadHalf, OwnedWriteHalf>,
    TP: volo_thrift::tracing::SpanProvider,
    GI: Layer<volo_grpc::server::Router>,
    GI::Service: Service<
            volo_grpc::context::ServerContext,
            volo_grpc::Request<BoxBody>,
            Response = volo_grpc::Response<BoxBody>,
        > + Clone
        + Send
        + Sync
        + 'static,
    <GI::Service as Service<volo_grpc::context::ServerContext, volo_grpc::Request<BoxBody>>>::Error:
        Into<Status>,
    GO: tower::Layer<GrpcLayerService>,
    GO::Service: tower::Service<http::Request<BoxBody>, Response = http::Response<BoxBody>>
        + Clone
        + Send
        + 'static,
    <GO::Service as tower::Service<http::Request<BoxBody>>>::Future: Send + 'static,
    <GO::Service as tower::Service<http::Request<BoxBody>>>::Error:
        Into<Status> + Send + Sync + std::error::Error + 'static,
    GP: volo_grpc::tracing::SpanProvider,
{
    pub fn prepare(self) -> Result<NativePrepared<TL::Service, C, TP, GO::Service>, BoxError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let directory = self.registry.freeze()?;
        let available = directory.protocols();
        let protocols = self.protocols.unwrap_or(available);
        if protocols.0 == 0 || protocols.0 & !available.0 != 0 {
            return Err(invalid(
                "enabled protocols must be a nonempty subset of registered capabilities",
            )
            .into());
        }
        let grpc = self.grpc.with_directory(directory.clone())?.prepare();
        grpc.validate_streamx()?;
        #[cfg(feature = "__tls")]
        if let Some(config) = grpc.tls_config() {
            let allowed: &[&[u8]] = if protocols.contains(Protocol::Grpc) {
                &[b"h2"]
            } else {
                &[]
            };
            config.acceptor.validate_alpn(allowed)?;
        }
        let streams = protocols
            .contains(Protocol::Ttstream)
            .then(|| directory.ttstream_router());
        let thrift = self
            .thrift
            .with_directory(directory.clone(), streams)?
            .prepare()?;
        if protocols.contains(Protocol::Thrift) {
            thrift.connection().validate_buffered_handoff()?;
        }
        let middleware = if !self.has_layers && self.hooks.is_empty() {
            None
        } else {
            let service = self.has_layers.then(|| {
                volo::streaming::InvocationService::new(
                    self.layer.layer(volo::streaming::InvocationHandler),
                )
            });
            Some(volo::streaming::ServerMiddleware::new(service, self.hooks))
        };
        Ok(PreparedServer {
            thrift,
            grpc,
            directory,
            protocols,
            state: Arc::new(State::new(
                self.observer,
                self.connection_timeout,
                middleware,
            )),
        })
    }

    pub async fn run<A: MakeIncoming>(self, incoming: A) -> Result<(), BoxError> {
        self.prepare()?.run(incoming).await
    }
    pub async fn run_with_shutdown<A: MakeIncoming, F: Future<Output = io::Result<()>>>(
        self,
        incoming: A,
        signal: F,
    ) -> Result<(), BoxError> {
        self.prepare()?.run_with_shutdown(incoming, signal).await
    }
}

impl<S, C, P, G> PreparedServer<NativeThrift<S, C, P>, volo_grpc::server::PreparedServer<G>>
where
    S: Service<
            volo_thrift::context::ServerContext,
            RoutedRequest,
            Response = RoutedResponse,
            Error = volo_thrift::ServerError,
        > + Send
        + Sync
        + 'static,
    C: MakeCodec<OwnedReadHalf, OwnedWriteHalf>,
    P: volo_thrift::tracing::SpanProvider,
    G: tower::Service<http::Request<BoxBody>, Response = http::Response<BoxBody>>
        + Clone
        + Send
        + 'static,
    G::Future: Send + 'static,
    G::Error: Into<Status> + Send + Sync + std::error::Error + 'static,
{
    pub async fn run<A: MakeIncoming>(self, incoming: A) -> Result<(), BoxError> {
        self.run_with_shutdown(incoming, shutdown_signal()).await
    }

    pub async fn run_with_shutdown<A: MakeIncoming, F: Future<Output = io::Result<()>>>(
        mut self,
        incoming: A,
        signal: F,
    ) -> Result<(), BoxError> {
        if self.grpc.uses_tls() && incoming.supports_tls() == Some(false) {
            return Err(
                invalid("StreamX TCP TLS cannot be combined with a non-TCP incoming").into(),
            );
        }
        let mut stopped = self.state.stop.subscribe();
        let mut owner = RunOwner {
            state: self.state.clone(),
            connections: JoinSet::new(),
            runtime: tokio::runtime::Handle::current(),
            timeout: self.grpc.shutdown_timeout(),
            armed: true,
        };
        tokio::pin!(signal);
        let incoming = tokio::select! { biased;
            _ = wait_stop(&mut stopped) => None,
            result = &mut signal => { result?; None },
            incoming = incoming.make_incoming() => Some(incoming?),
        };
        let Some(mut incoming) = incoming else {
            let _ = tokio::time::timeout(
                self.grpc.shutdown_timeout(),
                self.thrift.run_shutdown_hooks(),
            )
            .await;
            return Ok(());
        };
        let (shutdown, shutdown_rx) = watch::channel(());
        let outcome = loop {
            tokio::select! { biased;
                _ = wait_stop(&mut stopped) => break Ok(()),
                result = &mut signal => break result,
                Some(result) = owner.connections.join_next(), if !owner.connections.is_empty() => {
                    if let Err(error) = result { tracing::debug!("StreamX connection task: {error}"); }
                }
                accepted = incoming.accept() => match accepted {
                    Ok(Some(conn)) => {
                        let thrift = self.thrift.connection();
                        let grpc = self.grpc.clone();
                        let protocols = self.protocols;
                        let state = self.state.clone();
                        let guard = ConnectionGuard::new(state.clone());
                        let shutdown = shutdown_rx.clone();
                        owner.connections.spawn(async move {
                            let drain = state.drain.clone();
                            let middleware = state.middleware.clone();
                            // Keep the large protocol future out of the outer task state. Removing this
                            // indirection can overflow worker stacks with deeply layered services.
                            let connection = Box::pin(Self::serve_connection(conn, thrift, grpc, protocols, state, shutdown, guard));
                            volo::streaming::ServerMiddleware::scope_optional(middleware, drain.scope(connection)).await;
                        });
                    }
                    Ok(None) => break Ok(()),
                    Err(error) => break Err(error),
                }
            }
        };
        drop(incoming);
        let deadline = tokio::time::Instant::now() + self.grpc.shutdown_timeout();
        self.state.stop.send_replace(true);
        let control = self.thrift.control();
        control.begin_drain();
        control.close_idle();
        shutdown.send_replace(());
        let drain = async {
            self.thrift.run_shutdown_hooks().await;
            while let Some(result) = owner.connections.join_next().await {
                if let Err(error) = result {
                    tracing::debug!("StreamX draining connection: {error}");
                }
            }
        };
        if tokio::time::timeout_at(deadline, drain).await.is_err() {
            let drivers = self.state.drain.cancel_all();
            owner.connections.abort_all();
            let reclaim = async {
                while owner.connections.join_next().await.is_some() {}
                for driver in drivers.into_iter().chain(self.state.drain.cancel_all()) {
                    driver.reclaimed().await;
                }
            };
            let _ = tokio::time::timeout(owner.timeout, reclaim).await;
        }
        owner.armed = false;
        outcome.map_err(Into::into)
    }

    async fn serve_connection(
        conn: Conn,
        thrift: ThriftDriver<S, C, P>,
        grpc: volo_grpc::server::PreparedServer<G>,
        protocols: Protocols,
        state: Arc<State>,
        shutdown: watch::Receiver<()>,
        mut guard: ConnectionGuard,
    ) {
        let mut stopped = state.stop.subscribe();
        let detection_deadline = tokio::time::Instant::now() + state.connection_timeout;
        let _close_guard = conn.close_guard();
        if grpc.uses_tls()
            && !matches!(
                conn.stream.transport(),
                ConnectionTransport::Tcp | ConnectionTransport::Tls
            )
        {
            state.reject(Rejection::TlsTransport);
            return;
        }
        let conn = tokio::select! { biased;
            _ = wait_stop(&mut stopped) => return,
            _ = tokio::time::sleep_until(detection_deadline) => { state.reject(Rejection::Timeout); return; },
            result = grpc.accept_streamx_connection(conn) => match result {
                Ok(conn) => conn,
                Err(_) => { state.reject(Rejection::TlsHandshake); return; }
            },
        };
        if *stopped.borrow() {
            return;
        }
        let alpn_h2 = match conn.stream.negotiated_alpn() {
            None => false,
            Some(alpn) if alpn == b"h2" && protocols.contains(Protocol::Grpc) => true,
            _ => {
                state.reject(Rejection::Alpn);
                return;
            }
        };
        let transport = conn.stream.transport();
        let (protocol, input, detected) = if let Some(protocol) = protocols.single() {
            if alpn_h2 && protocol != Protocol::Grpc {
                state.reject(Rejection::Alpn);
                return;
            }
            (protocol, Input::Direct(conn), false)
        } else {
            state.detected.fetch_add(1, Ordering::Relaxed);
            let peeked = tokio::select! { biased;
                _ = wait_stop(&mut stopped) => return,
                _ = tokio::time::sleep_until(detection_deadline) => { state.reject(Rejection::Timeout); return; },
                result = PeekedConn::peek_until(conn, |prefix| {
                    prefix.len() >= 6 && &prefix[..6] != b"PRI * "
                        && prefix[4..6] != [0x10, 0x00]
                }) => result,
            };
            let peeked = match peeked {
                Ok(Some(conn)) => conn,
                Ok(None) => {
                    state.reject(Rejection::Empty);
                    return;
                }
                Err(error) => {
                    state.reject(if error.kind() == io::ErrorKind::UnexpectedEof {
                        Rejection::ShortPrefix
                    } else {
                        Rejection::Io
                    });
                    return;
                }
            };
            let protocol = classify(peeked.prefix());
            if !protocols.contains(protocol) {
                state.reject(Rejection::DisabledProtocol);
                return;
            }
            if alpn_h2 && protocol != Protocol::Grpc {
                state.reject(Rejection::Alpn);
                return;
            }
            (protocol, Input::Detected(peeked), true)
        };
        guard.selected = Some(protocol);
        match protocol {
            Protocol::Thrift => &state.thrift,
            Protocol::Ttstream => &state.ttstream,
            Protocol::Grpc => &state.grpc,
        }
        .fetch_add(1, Ordering::Relaxed);
        state.event(ConnectionEvent::Selected {
            protocol,
            transport,
            detected,
        });
        let result = match (protocol, input) {
            (Protocol::Grpc, Input::Direct(conn)) => grpc.serve_selected(conn, shutdown).await,
            (Protocol::Grpc, Input::Detected(conn)) => grpc.serve_detected(conn, shutdown).await,
            (protocol, input) => {
                let capacity = input.read_capacity();
                let parts = match input {
                    Input::Direct(conn) => BufferedConnParts::from_conn(conn, capacity),
                    Input::Detected(conn) => conn.into_buffered_split(capacity),
                };
                thrift
                    .serve_detected(
                        parts,
                        if protocol == Protocol::Thrift {
                            DetectedThrift::Unary
                        } else {
                            DetectedThrift::Ttstream
                        },
                    )
                    .await
            }
        };
        if let Err(error) = result {
            state.driver_errors.fetch_add(1, Ordering::Relaxed);
            tracing::debug!("StreamX {protocol:?} driver: {error}");
        }
    }
}

fn classify(prefix: &[u8; 8]) -> Protocol {
    if prefix == b"PRI * HT" {
        Protocol::Grpc
    } else if u16::from_be_bytes([prefix[4], prefix[5]]) == 0x1000
        && u16::from_be_bytes([prefix[6], prefix[7]]) & 2 != 0
    {
        Protocol::Ttstream
    } else {
        Protocol::Thrift
    }
}
enum Input {
    Direct(Conn),
    Detected(PeekedConn),
}
impl Input {
    fn read_capacity(&self) -> usize {
        let transport = match self {
            Self::Direct(conn) => conn.stream.transport(),
            Self::Detected(conn) => conn.transport(),
        };
        if transport == ConnectionTransport::Shmipc {
            return 512;
        }
        8192
    }
}
struct ConnectionGuard {
    state: Arc<State>,
    selected: Option<Protocol>,
}
impl ConnectionGuard {
    fn new(state: Arc<State>) -> Self {
        state.accepted.fetch_add(1, Ordering::Relaxed);
        state.active.fetch_add(1, Ordering::Relaxed);
        Self {
            state,
            selected: None,
        }
    }
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.state.active.fetch_sub(1, Ordering::Relaxed);
        self.state.event(ConnectionEvent::Closed(self.selected));
    }
}
struct RunOwner {
    state: Arc<State>,
    connections: JoinSet<()>,
    runtime: tokio::runtime::Handle,
    timeout: Duration,
    armed: bool,
}
impl Drop for RunOwner {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let drivers = self.state.drain.cancel_all();
        self.state.stop.send_replace(true);
        self.connections.abort_all();
        let mut connections = std::mem::take(&mut self.connections);
        let state = self.state.clone();
        let timeout = self.timeout;
        // A captured handle also permits dropping the run future outside a
        // currently entered runtime. Join aborted accepts before the final
        // snapshot, so a driver registered by an in-flight poll is reclaimed.
        self.runtime.spawn(async move {
            let reclaim = async {
                while connections.join_next().await.is_some() {}
                for driver in drivers.into_iter().chain(state.drain.cancel_all()) {
                    driver.reclaimed().await;
                }
            };
            let _ = tokio::time::timeout(timeout, reclaim).await;
        });
    }
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
async fn wait_stop(receiver: &mut watch::Receiver<bool>) {
    loop {
        if *receiver.borrow_and_update() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}
async fn shutdown_signal() -> io::Result<()> {
    #[cfg(target_family = "unix")]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut hangup = signal(SignalKind::hangup())?;
        let mut terminate = signal(SignalKind::terminate())?;
        tokio::select! { _ = interrupt.recv() => {}, _ = hangup.recv() => {}, _ = terminate.recv() => {} }
        Ok(())
    }
    #[cfg(not(target_family = "unix"))]
    {
        tokio::signal::ctrl_c().await
    }
}

#[cfg(test)]
#[path = "layer_tests.rs"]
mod layer_tests;

#[cfg(test)]
mod review_tests {
    use super::*;

    #[test]
    fn run_owner_can_be_dropped_outside_an_entered_runtime() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let state = Arc::new(State::new(None, Duration::from_secs(1), None));
        let owner = runtime.block_on(async {
            RunOwner {
                state: state.clone(),
                connections: JoinSet::new(),
                runtime: tokio::runtime::Handle::current(),
                timeout: Duration::from_secs(1),
                armed: true,
            }
        });
        assert!(tokio::runtime::Handle::try_current().is_err());
        drop(owner);
        assert!(*state.stop.borrow());
        runtime.block_on(tokio::task::yield_now());
    }
}
