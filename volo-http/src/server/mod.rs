use std::{
    cell::RefCell,
    convert::Infallible,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use futures::future::BoxFuture;
use http_body::Body;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use metainfo::{MetaInfo, METAINFO};
use motore::{
    layer::{Identity, Layer, Stack},
    service::Service,
    BoxError,
};
use scopeguard::defer;
use tokio::sync::Notify;
use tracing::{info, trace};
use volo::net::{conn::Conn, incoming::Incoming, Address, MakeIncoming};

use crate::{
    context::{server::Config, ServerContext},
    request::ServerRequest,
    response::ServerResponse,
};

mod into_response;
pub use self::into_response::IntoResponse;

pub mod extract;
mod handler;
pub mod layer;
pub mod middleware;
pub mod param;
pub mod route;

#[doc(hidden)]
pub mod prelude {
    pub use super::{param::Params, route::Router, Server};
    #[cfg(feature = "cookie")]
    pub use crate::cookie::CookieJar;
}

/// This is unstable now and may be changed in the future.
#[doc(hidden)]
type TraceFn = fn(&ServerContext);

pub struct Server<S, L> {
    service: S,
    layer: L,
    config: Config,
    http_config: ServerConfig,
    stat_tracer: Vec<TraceFn>,
    shutdown_hooks: Vec<Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>>,
}

impl<S> Server<S, Identity> {
    /// Create a new server.
    pub fn new(service: S) -> Self {
        Self {
            service,
            layer: Identity::new(),
            config: Config::default(),
            http_config: ServerConfig::default(),
            stat_tracer: Vec::new(),
            shutdown_hooks: Vec::new(),
        }
    }
}

impl<S, L> Server<S, L> {
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
    pub fn layer<Inner>(self, layer: Inner) -> Server<S, Stack<Inner, L>> {
        Server {
            service: self.service,
            layer: Stack::new(layer, self.layer),
            config: self.config,
            http_config: self.http_config,
            stat_tracer: self.stat_tracer,
            shutdown_hooks: self.shutdown_hooks,
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
    pub fn layer_front<Front>(self, layer: Front) -> Server<S, Stack<L, Front>> {
        Server {
            service: self.service,
            layer: Stack::new(self.layer, layer),
            config: self.config,
            http_config: self.http_config,
            stat_tracer: self.stat_tracer,
            shutdown_hooks: self.shutdown_hooks,
        }
    }

    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    pub fn stat_tracer(mut self, trace_fn: TraceFn) -> Self {
        self.stat_tracer.push(trace_fn);
        self
    }

    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }

    /// Get a reference to the HTTP configuration of the client.
    pub fn http_config(&self) -> &ServerConfig {
        &self.http_config
    }

    /// Get a mutable reference to the HTTP configuration of the client.
    pub fn http_config_mut(&mut self) -> &mut ServerConfig {
        &mut self.http_config
    }

    /// Set whether HTTP/1 connections should support half-closures.
    ///
    /// Clients can chose to shutdown their write-side while waiting
    /// for the server to respond. Setting this to `true` will
    /// prevent closing the connection immediately if `read`
    /// detects an EOF in the middle of a request.
    ///
    /// Default is `false`.
    pub fn set_half_close(&mut self, half_close: bool) -> &mut Self {
        self.http_config.half_close = half_close;
        self
    }

    /// Enables or disables HTTP/1 keep-alive.
    ///
    /// Default is true.
    pub fn set_keep_alive(&mut self, keep_alive: bool) -> &mut Self {
        self.http_config.keep_alive = keep_alive;
        self
    }

    /// Set whether HTTP/1 connections will write header names as title case at
    /// the socket level.
    ///
    /// Default is false.
    pub fn set_title_case_headers(&mut self, title_case_headers: bool) -> &mut Self {
        self.http_config.title_case_headers = title_case_headers;
        self
    }

    /// Set whether to support preserving original header cases.
    ///
    /// Currently, this will record the original cases received, and store them
    /// in a private extension on the `Request`. It will also look for and use
    /// such an extension in any provided `Response`.
    ///
    /// Since the relevant extension is still private, there is no way to
    /// interact with the original cases. The only effect this can have now is
    /// to forward the cases in a proxy-like fashion.
    ///
    /// Default is false.
    pub fn set_preserve_header_case(&mut self, preserve_header_case: bool) -> &mut Self {
        self.http_config.preserve_header_case = preserve_header_case;
        self
    }

    /// Set the maximum number of headers.
    ///
    /// When a request is received, the parser will reserve a buffer to store headers for optimal
    /// performance.
    ///
    /// If server receives more headers than the buffer size, it responds to the client with
    /// "431 Request Header Fields Too Large".
    ///
    /// Note that headers is allocated on the stack by default, which has higher performance. After
    /// setting this value, headers will be allocated in heap memory, that is, heap memory
    /// allocation will occur for each request, and there will be a performance drop of about 5%.
    ///
    /// Default is 100.
    pub fn set_max_headers(&mut self, max_headers: usize) -> &mut Self {
        self.http_config.max_headers = Some(max_headers);
        self
    }

    /// The main entry point for the server.
    pub async fn run<MI>(self, mk_incoming: MI) -> Result<(), BoxError>
    where
        S: Service<ServerContext, ServerRequest, Error = Infallible>,
        S::Response: IntoResponse,
        L: Layer<S>,
        L::Service:
            Service<ServerContext, ServerRequest, Error = Infallible> + Send + Sync + 'static,
        <L::Service as Service<ServerContext, ServerRequest>>::Response: IntoResponse,
        MI: MakeIncoming,
    {
        // init server
        let service = Arc::new(self.layer.layer(self.service));
        // TODO(lyf1999): type annotation is needed here, figure out why
        let stat_tracer: Arc<[TraceFn]> = Arc::from(self.stat_tracer);

        let mut incoming = mk_incoming.make_incoming().await?;
        info!("[VOLO] server start at: {:?}", incoming);

        let conn_cnt = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let gconn_cnt = conn_cnt.clone();
        let (exit_notify, exit_flag) = (
            Arc::new(Notify::const_new()),
            Arc::new(parking_lot::RwLock::new(false)),
        );
        let (exit_notify_inner, exit_flag_inner) = (exit_notify.clone(), exit_flag.clone());

        // spawn accept loop
        let handler = tokio::spawn(async move {
            let exit_flag = exit_flag_inner.clone();
            loop {
                if *exit_flag.read() {
                    break Ok(());
                }
                match incoming.accept().await {
                    Ok(Some(conn)) => {
                        let peer = conn
                            .info
                            .peer_addr
                            .clone()
                            .expect("http address should have one");

                        trace!("[VOLO] accept connection from: {:?}", peer);

                        tokio::task::spawn(handle_conn(
                            conn,
                            service.clone(),
                            self.config,
                            stat_tracer.clone(),
                            exit_notify_inner.clone(),
                            conn_cnt.clone(),
                            peer,
                        ));
                    }
                    Ok(None) => break Ok(()),
                    Err(e) => break Err(Box::new(e)),
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
}

pub struct ServerConfig {
    pub half_close: bool,
    pub keep_alive: bool,
    pub title_case_headers: bool,
    pub preserve_header_case: bool,
    pub max_headers: Option<usize>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerConfig {
    pub fn new() -> Self {
        Self {
            half_close: false,
            keep_alive: true,
            title_case_headers: false,
            preserve_header_case: false,
            max_headers: None,
        }
    }
}

async fn handle_conn<S>(
    conn: Conn,
    service: S,
    config: Config,
    stat_tracer: Arc<[TraceFn]>,
    exit_notify: Arc<Notify>,
    conn_cnt: Arc<std::sync::atomic::AtomicUsize>,
    peer: Address,
) where
    S: Service<ServerContext, ServerRequest, Error = Infallible> + Clone + Send + Sync + 'static,
    S::Response: IntoResponse,
{
    conn_cnt.fetch_add(1, Ordering::Relaxed);
    defer! {
        conn_cnt.fetch_sub(1, Ordering::Relaxed);
    }
    let notified = exit_notify.notified();
    tokio::pin!(notified);

    let mut http_conn = http1::Builder::new().serve_connection(
        TokioIo::new(conn),
        hyper::service::service_fn(|req| {
            serve(
                service.clone(),
                peer.clone(),
                config,
                stat_tracer.clone(),
                req,
            )
        }),
    );
    tokio::select! {
        _ = &mut notified => {
            tracing::trace!("[VOLO] closing a pending connection");
            // Graceful shutdown.
            hyper::server::conn::http1::Connection::graceful_shutdown(
                Pin::new(&mut http_conn)
            );
            // Continue to poll this connection until shutdown can finish.
            let result = http_conn.await;
            if let Err(err) = result {
                tracing::debug!("[VOLO] connection error: {:?}", err);
            }
        }
        result = &mut http_conn => {
            if let Err(err) = result {
                tracing::debug!("[VOLO] http connection error: {:?}", err);
            }
        },
    }
}

async fn serve<S>(
    service: S,
    peer: Address,
    config: Config,
    stat_tracer: Arc<[TraceFn]>,
    request: ServerRequest,
) -> Result<ServerResponse, Infallible>
where
    S: Service<ServerContext, ServerRequest, Error = Infallible> + Clone + Send + Sync + 'static,
    S::Response: IntoResponse,
{
    METAINFO
        .scope(RefCell::new(MetaInfo::default()), async {
            let service = service.clone();
            let peer = peer.clone();
            let mut cx = ServerContext::new(peer, config.stat_enable);

            if config.stat_enable {
                cx.stats.set_uri(request.uri().to_owned());
                cx.stats.set_method(request.method().to_owned());
                if let Some(req_size) = request.size_hint().exact() {
                    cx.common_stats.set_req_size(req_size);
                }
                cx.stats.record_process_start_at();
            }

            let resp = service.call(&mut cx, request).await.into_response();

            if config.stat_enable {
                cx.stats.record_process_end_at();
                cx.common_stats.set_status_code(resp.status());
                if let Some(resp_size) = resp.size_hint().exact() {
                    cx.common_stats.set_resp_size(resp_size);
                }

                stat_tracer.iter().for_each(|f| f(&cx));
            }

            Ok(resp)
        })
        .await
}
