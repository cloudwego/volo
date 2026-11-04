use std::{
    marker::PhantomData,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use futures::future::BoxFuture;
use motore::{
    BoxError,
    layer::{Identity, Layer, Stack},
    service::Service,
};
use scopeguard::defer;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::Notify,
};
use tracing::{info, trace};
#[cfg(feature = "shmipc")]
use volo::net::shmipc_fallback::ShmipcAddressWithFallback;
use volo::{
    net::{
        Address,
        conn::{OwnedReadHalf, OwnedWriteHalf},
        incoming::Incoming,
    },
    service::BoxService,
};

use crate::{
    EntryMessage,
    codec::{
        DefaultMakeCodec, MakeCodec,
        default::{framed::MakeFramedCodec, thrift::MakeThriftCodec, ttheader::MakeTTHeaderCodec},
    },
    context::ServerContext,
    server::layer::biz_error::BizErrorLayer,
    tracing::{DefaultProvider, SpanProvider},
};

mod layer;
pub mod panic_handler;
pub mod router;

pub use router::{NamedService, Router};

/// This is unstable now and may be changed in the future.
#[doc(hidden)]
pub type TraceFn = fn(&ServerContext);

pub struct Server<S, L, Req, MkC, SP> {
    service: S,
    streaming_router: Option<crate::transport::ttstream::connection::StreamRouter>,
    streaming_config: crate::streaming::StreamConfig,
    streaming_hooks: Option<crate::streaming::HookChain>,
    streaming_scopes: Vec<Arc<dyn crate::streaming::StreamHandlerScope>>,
    layer: L,
    make_codec: MkC,
    stat_tracer: Vec<TraceFn>,
    #[cfg(feature = "multiplex")]
    multiplex: bool,
    span_provider: SP,
    shutdown_hooks: Vec<Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>>,
    _marker: PhantomData<Req>,
}

impl<S, Req>
    Server<
        S,
        Identity,
        Req,
        DefaultMakeCodec<MakeTTHeaderCodec<MakeFramedCodec<MakeThriftCodec>>>,
        DefaultProvider,
    >
{
    pub fn new(service: S) -> Self
    where
        S: Service<ServerContext, Req>,
    {
        Self {
            make_codec: DefaultMakeCodec::default(),
            service,
            streaming_router: None,
            streaming_config: Default::default(),
            streaming_hooks: None,
            streaming_scopes: Vec::new(),
            layer: Identity::new(),
            stat_tracer: Vec::new(),
            #[cfg(feature = "multiplex")]
            multiplex: false,
            span_provider: DefaultProvider {},
            shutdown_hooks: Vec::new(),
            _marker: PhantomData,
        }
    }
}

impl
    Server<
        Router,
        Identity,
        bytes::Bytes,
        DefaultMakeCodec<MakeTTHeaderCodec<MakeFramedCodec<MakeThriftCodec>>>,
        DefaultProvider,
    >
{
    /// Creates a new server with a multi-service router.
    ///
    /// Use this method when you need to serve multiple Thrift services from a single server.
    /// Requests are routed based on the IDL service name (`isn`) field in TTHeader.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use volo_thrift::server::{Router, Server};
    ///
    /// let service_a = volo_gen::a::ServiceAServer::new(ImplA);
    /// let service_b = volo_gen::b::ServiceBServer::new(ImplB);
    ///
    /// let router = Router::new()
    ///     .with_default_service(service_a)
    ///     .add_service(service_b);
    ///
    /// Server::with_router(router)
    ///     .run(addr)
    ///     .await?;
    /// ```
    pub fn with_router(router: Router) -> Self {
        Self {
            make_codec: DefaultMakeCodec::default(),
            streaming_router: router.streaming_router(),
            streaming_config: Default::default(),
            streaming_hooks: None,
            streaming_scopes: Vec::new(),
            service: router,
            layer: Identity::new(),
            stat_tracer: Vec::new(),
            #[cfg(feature = "multiplex")]
            multiplex: false,
            span_provider: DefaultProvider {},
            shutdown_hooks: Vec::new(),
            _marker: PhantomData,
        }
    }
}

impl<S, L, Req, MkC, SP> Server<S, L, Req, MkC, SP> {
    /// Whether the final registered service set includes streaming methods.
    pub fn has_streaming_methods(&self) -> bool {
        self.streaming_router.is_some()
    }

    /// Register the generated streaming dispatcher alongside the original unary service.
    pub fn streaming_router(
        mut self,
        router: crate::transport::ttstream::connection::StreamRouter,
    ) -> Self {
        self.streaming_router = router.has_streaming_methods().then_some(router);
        self
    }
    pub fn streaming_scope(mut self, scope: Arc<dyn crate::streaming::StreamHandlerScope>) -> Self {
        self.streaming_scopes.push(scope);
        self
    }
    /// Prepends a handler scope without enabling streaming for a pure unary service.
    pub fn streaming_scope_front(
        mut self,
        scope: Arc<dyn crate::streaming::StreamHandlerScope>,
    ) -> Self {
        self.streaming_scopes.insert(0, scope);
        self
    }
    /// Prepends a hook without enabling streaming for a pure unary service.
    pub fn streaming_hook_front(mut self, hook: Arc<dyn crate::streaming::StreamHook>) -> Self {
        self.streaming_hooks
            .get_or_insert_with(Default::default)
            .push_front(hook);
        self
    }
    pub fn streaming_hook(mut self, hook: Arc<dyn crate::streaming::StreamHook>) -> Self {
        self.streaming_hooks
            .get_or_insert_with(Default::default)
            .push(hook);
        self
    }
    pub fn streaming_config(mut self, config: crate::streaming::StreamConfig) -> Self {
        self.streaming_config = config;
        self
    }
    pub fn streaming_hooks(mut self, hooks: crate::streaming::HookChain) -> Self {
        self.streaming_hooks = (!hooks.is_empty()).then_some(hooks);
        self
    }

    /// Register shutdown hook.
    ///
    /// Hook functions will be called just before volo's own gracefull existing code starts,
    /// in reverse order of registration.
    pub fn register_shutdown_hook(
        mut self,
        hook: impl FnOnce() -> BoxFuture<'static, ()> + 'static + Send,
    ) -> Self {
        self.shutdown_hooks.push(Box::new(hook));
        self
    }

    /// Adds a new inner layer to the server.
    ///
    /// The layer's `Service` should be `Send + Sync + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer(baz)`, we will get: foo -> bar -> baz.
    pub fn layer<Inner>(self, layer: Inner) -> Server<S, Stack<Inner, L>, Req, MkC, SP> {
        Server {
            layer: Stack::new(layer, self.layer),
            service: self.service,
            streaming_router: self.streaming_router,
            streaming_config: self.streaming_config,
            streaming_hooks: self.streaming_hooks,
            streaming_scopes: self.streaming_scopes,
            make_codec: self.make_codec,
            stat_tracer: self.stat_tracer,
            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
            span_provider: self.span_provider,
            shutdown_hooks: self.shutdown_hooks,
            _marker: PhantomData,
        }
    }

    /// Adds a new front layer to the server.
    ///
    /// The layer's `Service` should be `Send + Sync + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer_front(baz)`, we will get: baz -> foo -> bar.
    pub fn layer_front<Front>(self, layer: Front) -> Server<S, Stack<L, Front>, Req, MkC, SP> {
        Server {
            layer: Stack::new(self.layer, layer),
            service: self.service,
            streaming_router: self.streaming_router,
            streaming_config: self.streaming_config,
            streaming_hooks: self.streaming_hooks,
            streaming_scopes: self.streaming_scopes,
            make_codec: self.make_codec,
            stat_tracer: self.stat_tracer,
            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
            span_provider: self.span_provider,
            shutdown_hooks: self.shutdown_hooks,
            _marker: PhantomData,
        }
    }

    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    pub fn stat_tracer(mut self, trace_fn: TraceFn) -> Self {
        self.stat_tracer.push(trace_fn);
        self
    }

    /// Set the codec to use for the server.
    ///
    /// This should not be used by most users, Volo has already provided a default encoder.
    /// This is only useful if you want to customize some protocol.
    ///
    /// If you only want to transform metadata across microservices, you can use [`metainfo`] to do
    /// this.
    #[doc(hidden)]
    pub fn make_codec<MakeCodec>(self, make_codec: MakeCodec) -> Server<S, L, Req, MakeCodec, SP> {
        Server {
            layer: self.layer,
            service: self.service,
            streaming_router: self.streaming_router,
            streaming_config: self.streaming_config,
            streaming_hooks: self.streaming_hooks,
            streaming_scopes: self.streaming_scopes,
            make_codec,
            stat_tracer: self.stat_tracer,
            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
            span_provider: self.span_provider,
            shutdown_hooks: self.shutdown_hooks,
            _marker: PhantomData,
        }
    }

    /// The main entry point for the server.
    pub async fn run<MI: volo::net::incoming::MakeIncoming>(
        mut self,
        make_incoming: MI,
    ) -> Result<(), BoxError>
    where
        L: Layer<BoxService<ServerContext, Req, S::Response, crate::ServerError>>,
        MkC: MakeCodec<OwnedReadHalf, OwnedWriteHalf>,
        L::Service: Service<ServerContext, Req, Response = S::Response, Error = crate::ServerError>
            + Send
            + 'static
            + Sync,
        S: Service<ServerContext, Req, Error = crate::ServerError> + Send + 'static + Sync,
        S::Response: EntryMessage + Send + 'static + Sync,
        Req: EntryMessage + Send + 'static,
        SP: SpanProvider,
    {
        if let Some(router) = &mut self.streaming_router {
            for scope in self.streaming_scopes.drain(..) {
                router.handler_scope(scope);
            }
            if !cfg!(feature = "ttstream") {
                return Err(Box::new(crate::streaming::StreamError::Configuration(
                    "ttstream feature is disabled".into(),
                )));
            }
            if !self.make_codec.supports_streaming() {
                return Err(Box::new(crate::streaming::StreamError::Configuration(
                    "custom MakeCodec must opt into the streaming reader adapter".into(),
                )));
            }
            self.streaming_config.validate()?;
            router.validate()?;
        }
        // init server
        // inject biz error layer first
        let service = Arc::new(
            self.layer
                .layer(BoxService::new(BizErrorLayer::new().layer(self.service))),
        );
        // TODO(lyf1999): type annotation is needed here, figure out why
        let stat_tracer: Arc<[TraceFn]> = Arc::from(self.stat_tracer);

        let mut incoming = make_incoming.make_incoming().await?;
        info!("[VOLO] server start at: {:?}", incoming);

        let conn_cnt = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let gconn_cnt = conn_cnt.clone();
        let (exit_notify, exit_flag, exit_mark) = (
            Arc::new(Notify::const_new()),
            Arc::new(parking_lot::RwLock::new(false)),
            Arc::new(std::sync::atomic::AtomicBool::default()),
        );
        let (exit_notify_inner, exit_flag_inner, exit_mark_inner) =
            (exit_notify.clone(), exit_flag.clone(), exit_mark.clone());

        // Streaming admission stops at the shutdown mark. Unary keeps its
        // existing crrst grace period and separate stop notification.
        let streaming = self.streaming_router.take().map(|router| {
            Arc::new(StreamingServerState {
                router,
                config: self.streaming_config,
                hooks: self.streaming_hooks,
                exit: Notify::new(),
            })
        });
        let streaming_inner = streaming.clone();
        // spawn accept loop
        let handler = tokio::spawn(async move {
            let exit_flag = exit_flag_inner.clone();
            loop {
                if *exit_flag.read() {
                    break Ok(());
                }
                match incoming.accept().await {
                    Ok(Some(conn)) => {
                        let peer_addr = conn.info.peer_addr;
                        trace!("[VOLO] accept connection from: {:?}", peer_addr);
                        let (rh, wh) = conn.stream.into_split();

                        #[cfg(feature = "multiplex")]
                        if self.multiplex {
                            #[cfg(feature = "shmipc")]
                            if peer_addr.as_ref().is_some_and(Address::is_shmipc)
                                && streaming_inner.is_none()
                            {
                                tracing::error!("multiplex is not supported when using shmipc");
                                let _ = rh.shmipc_helper().close().await;
                                continue;
                            }
                            if let Some(streaming) = &streaming_inner {
                                tokio::spawn(handle_conn_multiplex_streaming(
                                    rh,
                                    wh,
                                    service.clone(),
                                    self.make_codec.clone(),
                                    stat_tracer.clone(),
                                    exit_notify_inner.clone(),
                                    exit_mark_inner.clone(),
                                    conn_cnt.clone(),
                                    streaming.clone(),
                                    peer_addr,
                                ));
                            } else {
                                tokio::spawn(handle_conn_multiplex(
                                    rh,
                                    wh,
                                    service.clone(),
                                    self.make_codec.clone(),
                                    stat_tracer.clone(),
                                    exit_notify_inner.clone(),
                                    exit_mark_inner.clone(),
                                    conn_cnt.clone(),
                                    peer_addr,
                                ));
                            }
                        } else {
                            if let Some(streaming) = &streaming_inner {
                                tokio::spawn(handle_conn_streaming(
                                    rh,
                                    wh,
                                    service.clone(),
                                    self.make_codec.clone(),
                                    stat_tracer.clone(),
                                    exit_notify_inner.clone(),
                                    exit_mark_inner.clone(),
                                    conn_cnt.clone(),
                                    streaming.clone(),
                                    peer_addr,
                                    self.span_provider.clone(),
                                ));
                            } else {
                                tokio::spawn(handle_conn(
                                    rh,
                                    wh,
                                    service.clone(),
                                    self.make_codec.clone(),
                                    stat_tracer.clone(),
                                    exit_notify_inner.clone(),
                                    exit_mark_inner.clone(),
                                    conn_cnt.clone(),
                                    peer_addr,
                                    self.span_provider.clone(),
                                ));
                            }
                        }
                        #[cfg(not(feature = "multiplex"))]
                        if let Some(streaming) = &streaming_inner {
                            tokio::spawn(handle_conn_streaming(
                                rh,
                                wh,
                                service.clone(),
                                self.make_codec.clone(),
                                stat_tracer.clone(),
                                exit_notify_inner.clone(),
                                exit_mark_inner.clone(),
                                conn_cnt.clone(),
                                streaming.clone(),
                                peer_addr,
                                self.span_provider.clone(),
                            ));
                        } else {
                            tokio::spawn(handle_conn(
                                rh,
                                wh,
                                service.clone(),
                                self.make_codec.clone(),
                                stat_tracer.clone(),
                                exit_notify_inner.clone(),
                                exit_mark_inner.clone(),
                                conn_cnt.clone(),
                                peer_addr,
                                self.span_provider.clone(),
                            ));
                        }
                    }
                    // no more incoming connections
                    Ok(None) => break Ok(()),
                    Err(e) => break Err(e),
                }
            }
        });

        #[cfg(target_family = "unix")]
        {
            // graceful shutdown
            let mut sigint =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
            let mut sighup =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

            // graceful shutdown handler
            tokio::select! {
                _ = sigint.recv() => {}
                _ = sighup.recv() => {}
                _ = sigterm.recv() => {}
                res = handler => {
                    match res {
                        Ok(res) => {
                            match res {
                                Ok(()) => {}
                                Err(e) => return Err(Box::new(e))
                            };
                        }
                        Err(e) => return Err(Box::new(e)),
                    }
                }
            }
        }

        // graceful shutdown handler for windows
        #[cfg(target_family = "windows")]
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            res = handler => {
                match res {
                    Ok(res) => {
                        match res {
                            Ok(()) => {}
                            Err(e) => return Err(Box::new(e))
                        };
                    }
                    Err(e) => return Err(Box::new(e)),
                }
            }
        }

        if !self.shutdown_hooks.is_empty() {
            info!("[VOLO] call shutdown hooks");

            for hook in self.shutdown_hooks {
                (hook)().await;
            }
        }

        // received signal, graceful shutdown now
        info!("[VOLO] received signal, gracefully exiting now");
        *exit_flag.write() = true;
        exit_mark.store(true, Ordering::Release);
        if let Some(streaming) = &streaming {
            streaming.exit.notify_waiters();
        }

        // Now we won't accept new connections.
        // And we want to send crrst reply to the peers in the short future.
        if gconn_cnt.load(Ordering::Relaxed) != 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        exit_notify.notify_waiters();

        // wait for all connections to be closed
        for _ in 0..28 {
            if gconn_cnt.load(Ordering::Relaxed) == 0 {
                break;
            }
            trace!(
                "[VOLO] gracefully exiting, remaining connection count: {}",
                gconn_cnt.load(Ordering::Relaxed)
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Ok(())
    }

    #[cfg(feature = "shmipc")]
    /// Run the server with shmipc and TCP fallback support.
    ///
    /// The server will listen on both the shmipc address and the fallback TCP address.
    /// Clients can connect via either transport.
    pub async fn run_with_fallback<A1, A2>(
        self,
        shmipc_addr: A1,
        fallback_addr: A2,
    ) -> Result<(), BoxError>
    where
        A1: Into<Address>,
        A2: volo::net::incoming::MakeIncoming + Send,
        A2::Incoming: Send,
        L: Layer<BoxService<ServerContext, Req, S::Response, crate::ServerError>>,
        MkC: MakeCodec<OwnedReadHalf, OwnedWriteHalf>,
        L::Service: Service<ServerContext, Req, Response = S::Response, Error = crate::ServerError>
            + Send
            + 'static
            + Sync,
        S: Service<ServerContext, Req, Error = crate::ServerError> + Send + 'static + Sync,
        S::Response: EntryMessage + Send + 'static + Sync,
        Req: EntryMessage + Send + 'static,
        SP: SpanProvider,
    {
        self.run(ShmipcAddressWithFallback {
            shmipc_addr: shmipc_addr.into(),
            default_mi: fallback_addr,
        })
        .await
    }

    #[cfg(feature = "multiplex")]
    /// Use multiplexing to handle multiple requests in one connection.
    ///
    /// Not recommend for most users.
    #[doc(hidden)]
    pub fn multiplex(self, multiplex: bool) -> Server<S, L, Req, MkC, SP> {
        Server {
            layer: self.layer,
            service: self.service,
            streaming_router: self.streaming_router,
            streaming_config: self.streaming_config,
            streaming_hooks: self.streaming_hooks,
            streaming_scopes: self.streaming_scopes,
            make_codec: self.make_codec,
            stat_tracer: self.stat_tracer,
            multiplex,
            span_provider: self.span_provider,
            shutdown_hooks: self.shutdown_hooks,
            _marker: PhantomData,
        }
    }

    pub fn span_provider<P: SpanProvider>(self, provider: P) -> Server<S, L, Req, MkC, P> {
        Server {
            layer: self.layer,
            service: self.service,
            streaming_router: self.streaming_router,
            streaming_config: self.streaming_config,
            streaming_hooks: self.streaming_hooks,
            streaming_scopes: self.streaming_scopes,
            make_codec: self.make_codec,
            stat_tracer: self.stat_tracer,
            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
            span_provider: provider,
            shutdown_hooks: self.shutdown_hooks,
            _marker: PhantomData,
        }
    }
}

struct StreamingServerState {
    router: crate::transport::ttstream::connection::StreamRouter,
    config: crate::streaming::StreamConfig,
    hooks: Option<crate::streaming::HookChain>,
    exit: Notify,
}

#[allow(clippy::too_many_arguments)]
async fn handle_conn<R, W, Req, Svc, Resp, MkC, SP>(
    rh: R,
    wh: W,
    service: Svc,
    make_codec: MkC,
    stat_tracer: Arc<[TraceFn]>,
    exit_notify: Arc<Notify>,
    exit_mark: Arc<std::sync::atomic::AtomicBool>,
    conn_cnt: Arc<std::sync::atomic::AtomicUsize>,
    peer_addr: Option<Address>,
    span_provider: SP,
) where
    R: AsyncRead + Unpin + Send + Sync + 'static,
    W: AsyncWrite + Unpin + Send + Sync + 'static,
    Svc: Service<ServerContext, Req, Response = Resp> + Clone + Send + 'static,
    Svc::Error: Send,
    Svc::Error: Into<crate::ServerError>,
    Req: EntryMessage + Send + 'static,
    Resp: EntryMessage + Send + 'static,
    MkC: MakeCodec<R, W>,
    SP: SpanProvider,
{
    conn_cnt.fetch_add(1, Ordering::Relaxed);
    defer! {
        conn_cnt.fetch_sub(1, Ordering::Relaxed);
    }

    let (encoder, decoder) = make_codec.make_codec(rh, wh);

    #[cfg(feature = "shmipc")]
    let _guard = crate::codec::Decoder::shmipc_helper(&decoder).close_guard();

    tracing::trace!(
        "[VOLO] handle conn by ping-pong, peer_addr: {:?}",
        peer_addr
    );
    crate::transport::pingpong::serve(
        encoder,
        decoder,
        exit_notify.notified(),
        exit_mark,
        &service,
        stat_tracer,
        peer_addr,
        span_provider,
    )
    .await;
}

#[cfg(feature = "multiplex")]
#[allow(clippy::too_many_arguments)]
async fn handle_conn_multiplex<R, W, Req, Svc, Resp, MkC>(
    rh: R,
    wh: W,
    service: Svc,
    make_codec: MkC,
    stat_tracer: Arc<[TraceFn]>,
    exit_notify: Arc<Notify>,
    exit_mark: Arc<std::sync::atomic::AtomicBool>,
    conn_cnt: Arc<std::sync::atomic::AtomicUsize>,
    peer_addr: Option<Address>,
) where
    R: AsyncRead + Unpin + Send + Sync + 'static,
    W: AsyncWrite + Unpin + Send + Sync + 'static,
    Svc: Service<ServerContext, Req, Response = Resp> + Clone + Send + 'static + Sync,
    Svc::Error: Into<crate::ServerError> + Send,
    Req: EntryMessage + Send + 'static,
    Resp: EntryMessage + Send + 'static,
    MkC: MakeCodec<R, W>,
{
    conn_cnt.fetch_add(1, Ordering::Relaxed);
    defer! {
        conn_cnt.fetch_sub(1, Ordering::Relaxed);
    }
    let (encoder, decoder) = make_codec.make_codec(rh, wh);

    info!(
        "[VOLO] handle conn by multiplex, peer_addr: {:?}",
        peer_addr
    );
    crate::transport::multiplex::serve(
        encoder,
        decoder,
        exit_notify.notified(),
        exit_mark,
        service,
        stat_tracer,
        peer_addr,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn handle_conn_streaming<R, W, Req, Svc, Resp, MkC, SP>(
    rh: R,
    wh: W,
    service: Svc,
    make_codec: MkC,
    stat_tracer: Arc<[TraceFn]>,
    exit_notify: Arc<Notify>,
    exit_mark: Arc<std::sync::atomic::AtomicBool>,
    conn_cnt: Arc<std::sync::atomic::AtomicUsize>,
    streaming: Arc<StreamingServerState>,
    peer_addr: Option<Address>,
    span_provider: SP,
) where
    R: AsyncRead + Unpin + Send + Sync + 'static,
    W: AsyncWrite + Unpin + Send + Sync + 'static,
    Svc: Service<ServerContext, Req, Response = Resp> + Clone + Send + 'static,
    Svc::Error: Send,
    Svc::Error: Into<crate::ServerError>,
    Req: EntryMessage + Send + 'static,
    Resp: EntryMessage + Send + 'static,
    MkC: MakeCodec<R, W>,
    SP: SpanProvider,
{
    conn_cnt.fetch_add(1, Ordering::Relaxed);
    defer! {
        conn_cnt.fetch_sub(1, Ordering::Relaxed);
    }

    let (encoder, decoder) = make_codec.make_codec(rh, wh);
    let Some((encoder, decoder)) =
        dispatch_streaming(encoder, decoder, streaming, exit_mark.clone()).await
    else {
        return;
    };

    #[cfg(feature = "shmipc")]
    let _guard = crate::codec::Decoder::shmipc_helper(&decoder).close_guard();

    tracing::trace!(
        "[VOLO] handle conn by ping-pong, peer_addr: {:?}",
        peer_addr
    );
    crate::transport::pingpong::serve(
        encoder,
        decoder,
        exit_notify.notified(),
        exit_mark,
        &service,
        stat_tracer,
        peer_addr,
        span_provider,
    )
    .await;
}

#[cfg(feature = "multiplex")]
#[allow(clippy::too_many_arguments)]
async fn handle_conn_multiplex_streaming<R, W, Req, Svc, Resp, MkC>(
    rh: R,
    wh: W,
    service: Svc,
    make_codec: MkC,
    stat_tracer: Arc<[TraceFn]>,
    exit_notify: Arc<Notify>,
    exit_mark: Arc<std::sync::atomic::AtomicBool>,
    conn_cnt: Arc<std::sync::atomic::AtomicUsize>,
    streaming: Arc<StreamingServerState>,
    peer_addr: Option<Address>,
) where
    R: AsyncRead + Unpin + Send + Sync + 'static,
    W: AsyncWrite + Unpin + Send + Sync + 'static,
    Svc: Service<ServerContext, Req, Response = Resp> + Clone + Send + 'static + Sync,
    Svc::Error: Into<crate::ServerError> + Send,
    Req: EntryMessage + Send + 'static,
    Resp: EntryMessage + Send + 'static,
    MkC: MakeCodec<R, W>,
{
    conn_cnt.fetch_add(1, Ordering::Relaxed);
    defer! {
        conn_cnt.fetch_sub(1, Ordering::Relaxed);
    }
    let (encoder, decoder) = make_codec.make_codec(rh, wh);
    let Some((encoder, decoder)) =
        dispatch_streaming(encoder, decoder, streaming, exit_mark.clone()).await
    else {
        return;
    };

    // TTStream has its own mux and supports ShmIPC. Apply the existing unary
    // multiplex restriction only after mixed-listener dispatch has chosen unary.
    #[cfg(feature = "shmipc")]
    if peer_addr.as_ref().is_some_and(Address::is_shmipc) {
        tracing::error!("multiplex is not supported when using shmipc");
        let _ = crate::codec::Decoder::shmipc_helper(&decoder).close().await;
        return;
    }

    info!(
        "[VOLO] handle conn by multiplex, peer_addr: {:?}",
        peer_addr
    );
    crate::transport::multiplex::serve(
        encoder,
        decoder,
        exit_notify.notified(),
        exit_mark,
        service,
        stat_tracer,
        peer_addr,
    )
    .await;
}

async fn dispatch_streaming<E: crate::codec::Encoder, D: crate::codec::Decoder>(
    encoder: E,
    mut decoder: D,
    streaming: Arc<StreamingServerState>,
    exit_mark: Arc<std::sync::atomic::AtomicBool>,
) -> Option<(E, D)> {
    let shutdown = streaming_shutdown(&streaming.exit, &exit_mark);
    tokio::pin!(shutdown);
    let matched = tokio::select! { result=decoder.probe_ttstream()=>match result {Ok(value)=>value,Err(_)=>return None},_=&mut shutdown=>return None };
    if !matched {
        return Some((encoder, decoder));
    }
    #[cfg(feature = "shmipc")]
    let _guard = decoder.shmipc_helper().close_guard();
    let reader = match decoder.into_stream_reader() {
        Ok(reader) => reader,
        Err(_) => return None,
    };
    let writer = match encoder.into_stream_writer() {
        Ok(writer) => writer,
        Err(_) => return None,
    };
    let _ = crate::transport::ttstream::connection::Connection::serve(
        reader,
        writer,
        streaming.config.clone(),
        streaming.hooks.clone(),
        streaming.router.clone(),
        shutdown,
    )
    .await;
    None
}

async fn streaming_shutdown(exit: &Notify, mark: &std::sync::atomic::AtomicBool) {
    let notified = exit.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    if !mark.load(Ordering::Acquire) {
        notified.await;
    }
}

#[cfg(test)]
mod streaming_shutdown_tests {
    use super::*;
    #[tokio::test]
    async fn admission_shutdown_is_not_lost_before_or_during_first_frame_probe() {
        for early in [false, true] {
            let exit = Notify::new();
            let mark = std::sync::atomic::AtomicBool::new(early);
            if early {
                exit.notify_waiters();
            }
            let shutdown = streaming_shutdown(&exit, &mark);
            tokio::pin!(shutdown);
            if !early {
                assert!(futures::poll!(&mut shutdown).is_pending());
                mark.store(true, Ordering::Release);
                exit.notify_waiters();
            }
            assert!(futures::poll!(&mut shutdown).is_ready());
        }
    }
}
