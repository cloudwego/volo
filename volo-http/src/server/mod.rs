//! Server implementation
//!
//! See [`Server`] for more details.

use std::{
    cell::RefCell,
    convert::Infallible,
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::future::BoxFuture;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto,
};
use metainfo::{METAINFO, MetaInfo};
use motore::{
    BoxError,
    layer::{Identity, Layer, Stack},
    service::Service,
};
use parking_lot::RwLock;
use scopeguard::defer;
use tokio::sync::Notify;
use tracing::Instrument;
#[cfg(feature = "__tls")]
use volo::net::{conn::ConnStream, tls::ServerTlsConfig};
use volo::{
    context::Context,
    net::{Address, MakeIncoming, conn::Conn, incoming::Incoming},
};

use self::span_provider::{DefaultProvider, SpanProvider};
use crate::{
    body::Body,
    context::{ServerContext, server::Config},
    request::Request,
    response::Response,
};

pub mod extract;
mod handler;
pub mod layer;
pub mod middleware;
pub mod panic_handler;
pub mod param;
pub mod protocol;
pub mod response;
pub mod route;
pub mod span_provider;
#[cfg(test)]
pub mod test_helpers;
pub mod utils;

pub use self::{
    response::{IntoResponse, Redirect},
    route::Router,
};

#[doc(hidden)]
pub mod prelude {
    #[cfg(feature = "__tls")]
    pub use volo::net::tls::ServerTlsConfig;

    pub use super::{Server, param::PathParams, route::Router};
}

/// High level HTTP server.
///
/// # Examples
///
/// ```no_run
/// use std::net::SocketAddr;
///
/// use volo::net::Address;
/// use volo_http::server::{
///     Server,
///     route::{Router, get},
/// };
///
/// async fn index() -> &'static str {
///     "Hello, World!"
/// }
///
/// let app = Router::new().route("/", get(index));
/// let addr = "[::]:8080".parse::<SocketAddr>().unwrap();
/// let addr = Address::from(addr);
///
/// # tokio_test::block_on(async {
/// Server::new(app).run(addr).await.unwrap();
/// # })
/// ```
pub struct Server<S, L = Identity, SP = DefaultProvider> {
    service: S,
    layer: L,
    server: auto::Builder<TokioExecutor>,
    config: Config,
    shutdown_hooks: Vec<Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>>,
    span_provider: SP,
    #[cfg(feature = "__tls")]
    tls_config: Option<ServerTlsConfig>,
}

impl<S> Server<S, Identity, DefaultProvider> {
    /// Create a new server.
    pub fn new(service: S) -> Self {
        Self {
            service,
            layer: Identity::new(),
            server: auto::Builder::new(TokioExecutor::new()),
            config: Config::default(),
            shutdown_hooks: Vec::new(),
            span_provider: DefaultProvider,
            #[cfg(feature = "__tls")]
            tls_config: None,
        }
    }
}

impl<S, L, SP> Server<S, L, SP> {
    /// Enable TLS with the specified configuration.
    ///
    /// If not set, the server will not use TLS.
    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    pub fn tls_config(mut self, config: impl Into<ServerTlsConfig>) -> Self {
        self.tls_config = Some(config.into());
        self.config.set_tls(true);
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

    /// Add a new inner layer to the server.
    ///
    /// The layer's [`Service`] should be `Send + Sync + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer(baz)`, we will get: foo -> bar -> baz.
    pub fn layer<Inner>(self, layer: Inner) -> Server<S, Stack<Inner, L>, SP> {
        Server {
            service: self.service,
            layer: Stack::new(layer, self.layer),
            server: self.server,
            config: self.config,
            shutdown_hooks: self.shutdown_hooks,
            span_provider: self.span_provider,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Add a new front layer to the server.
    ///
    /// The layer's [`Service`] should be `Send + Sync + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer_front(baz)`, we will get: baz -> foo -> bar.
    pub fn layer_front<Front>(self, layer: Front) -> Server<S, Stack<L, Front>, SP> {
        Server {
            service: self.service,
            layer: Stack::new(self.layer, layer),
            server: self.server,
            config: self.config,
            shutdown_hooks: self.shutdown_hooks,
            span_provider: self.span_provider,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Set a [`SpanProvider`] to the server.
    ///
    /// Server will enter the [`Span`] that created by [`SpanProvider::on_serve`] when starting to
    /// serve a request, and call [`SpanProvider::leave_serve`] when leaving the serve function.
    ///
    /// [`Span`]: tracing::Span
    pub fn span_provider<P>(self, span_provider: P) -> Server<S, L, P> {
        Server {
            service: self.service,
            layer: self.layer,
            server: self.server,
            config: self.config,
            shutdown_hooks: self.shutdown_hooks,
            span_provider,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
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
    #[deprecated(
        since = "0.4.0",
        note = "`set_max_headers` has been removed into `http1_config`"
    )]
    #[cfg(feature = "http1")]
    pub fn set_max_headers(&mut self, max_headers: usize) -> &mut Self {
        self.server.http1().max_headers(max_headers);
        self
    }

    /// Get configuration for http1 part.
    #[cfg(feature = "http1")]
    pub fn http1_config(&mut self) -> self::protocol::Http1Config<'_> {
        self::protocol::Http1Config {
            inner: self.server.http1(),
        }
    }

    /// Get configuration for http2 part.
    #[cfg(feature = "http2")]
    pub fn http2_config(&mut self) -> self::protocol::Http2Config<'_> {
        self::protocol::Http2Config {
            inner: self.server.http2(),
        }
    }

    /// Make server accept only HTTP/1.
    #[cfg(feature = "http1")]
    pub fn http1_only(self) -> Self {
        Self {
            service: self.service,
            layer: self.layer,
            server: self.server.http1_only(),
            config: self.config,
            shutdown_hooks: self.shutdown_hooks,
            span_provider: self.span_provider,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Make server accept only HTTP/2.
    #[cfg(feature = "http2")]
    pub fn http2_only(self) -> Self {
        Self {
            service: self.service,
            layer: self.layer,
            server: self.server.http2_only(),
            config: self.config,
            shutdown_hooks: self.shutdown_hooks,
            span_provider: self.span_provider,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// The main entry point for the server.
    pub async fn run<MI, B>(self, mk_incoming: MI) -> Result<(), BoxError>
    where
        S: Service<ServerContext, Request<B>> + Send + Sync + 'static,
        S::Response: IntoResponse,
        S::Error: IntoResponse,
        L: Layer<S> + Send + Sync + 'static,
        L::Service: Service<ServerContext, Request, Error = Infallible> + Send + Sync + 'static,
        <L::Service as Service<ServerContext, Request>>::Response: IntoResponse,
        SP: SpanProvider + Clone + Send + Sync + Unpin + 'static,
        MI: MakeIncoming,
    {
        let server = Arc::new(self.server);
        let service = Arc::new(self.layer.layer(self.service));
        let incoming = mk_incoming.make_incoming().await?;
        tracing::info!("[Volo-HTTP] server start at: {:?}", incoming);

        // count connections, used for graceful shutdown
        let conn_cnt = Arc::new(AtomicUsize::new(0));
        // flag for stopping serve
        let exit_flag = Arc::new(parking_lot::RwLock::new(false));
        // notifier for stopping all inflight connections
        let exit_notify = Arc::new(Notify::const_new());

        let handler = tokio::spawn(serve(
            server,
            incoming,
            service,
            self.config,
            exit_flag.clone(),
            conn_cnt.clone(),
            exit_notify.clone(),
            self.span_provider,
            #[cfg(feature = "__tls")]
            self.tls_config,
        ));

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
                _ = handler => {},
            }
        }

        // graceful shutdown handler for windows
        #[cfg(target_family = "windows")]
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = handler => {},
        }

        if !self.shutdown_hooks.is_empty() {
            tracing::info!("[Volo-HTTP] call shutdown hooks");

            for hook in self.shutdown_hooks {
                (hook)().await;
            }
        }

        // received signal, graceful shutdown now
        tracing::info!("[Volo-HTTP] received signal, gracefully exiting now");
        *exit_flag.write() = true;

        // Now we won't accept new connections.
        // And we want to send crrst reply to the peers in the short future.
        if conn_cnt.load(Ordering::Relaxed) != 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        exit_notify.notify_waiters();

        // wait for all connections to be closed
        for _ in 0..28 {
            if conn_cnt.load(Ordering::Relaxed) == 0 {
                break;
            }
            tracing::trace!(
                "[Volo-HTTP] gracefully exiting, remaining connection count: {}",
                conn_cnt.load(Ordering::Relaxed),
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn serve<I, S, SP>(
    server: Arc<auto::Builder<TokioExecutor>>,
    mut incoming: I,
    service: S,
    config: Config,
    exit_flag: Arc<RwLock<bool>>,
    conn_cnt: Arc<AtomicUsize>,
    exit_notify: Arc<Notify>,
    span_provider: SP,
    #[cfg(feature = "__tls")] tls_config: Option<ServerTlsConfig>,
) where
    I: Incoming,
    S: Service<ServerContext, Request> + Clone + Unpin + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
    SP: SpanProvider + Clone + Send + Sync + Unpin + 'static,
{
    loop {
        if *exit_flag.read() {
            break;
        }

        let conn = match incoming.accept().await {
            Ok(Some(conn)) => conn,
            _ => continue,
        };
        #[cfg(feature = "__tls")]
        let conn = {
            let Conn { stream, info } = conn;
            match (stream, &tls_config) {
                (ConnStream::Tcp(stream), Some(tls_config)) => {
                    let stream = match tls_config.acceptor.accept(stream).await {
                        Ok(conn) => conn,
                        Err(err) => {
                            tracing::trace!("[Volo-HTTP] tls handshake error: {err:?}");
                            continue;
                        }
                    };
                    Conn { stream, info }
                }
                (stream, _) => Conn { stream, info },
            }
        };

        let peer = match conn.info.peer_addr {
            Some(ref peer) => {
                tracing::trace!("accept connection from: {peer:?}");
                peer.clone()
            }
            None => {
                tracing::info!("no peer address found from server connection");
                continue;
            }
        };

        let hyper_service = HyperService {
            inner: service.clone(),
            peer,
            config: config.clone(),
            span_provider: span_provider.clone(),
        };

        tokio::spawn(serve_conn(
            server.clone(),
            conn,
            hyper_service,
            conn_cnt.clone(),
            exit_notify.clone(),
        ));
    }
}

async fn serve_conn<S>(
    server: Arc<auto::Builder<TokioExecutor>>,
    conn: Conn,
    service: S,
    conn_cnt: Arc<AtomicUsize>,
    exit_notify: Arc<Notify>,
) where
    S: hyper::service::Service<HyperRequest, Response = Response> + Unpin,
    S::Future: Send + 'static,
    S::Error: Error + Send + Sync + 'static,
{
    conn_cnt.fetch_add(1, Ordering::Relaxed);
    defer! {
        conn_cnt.fetch_sub(1, Ordering::Relaxed);
    }

    let notified = exit_notify.notified();
    tokio::pin!(notified);

    let http_conn = server.serve_connection_with_upgrades(TokioIo::new(conn), service);
    futures::pin_mut!(http_conn);

    tokio::select! {
        _ = &mut notified => {
            tracing::trace!("[Volo-HTTP] closing a pending connection");
            // Graceful shutdown.
            http_conn.as_mut().graceful_shutdown();
            // Continue to poll this connection until shutdown can finish.
            let result = http_conn.as_mut().await;
            if let Err(err) = result {
                tracing::debug!("[Volo-HTTP] connection error: {:?}", err);
            }
        }
        result = http_conn.as_mut() => {
            if let Err(err) = result {
                tracing::debug!("[Volo-HTTP] connection error: {:?}", err);
            }
        },
    }
}

#[derive(Clone)]
struct HyperService<S, SP> {
    inner: S,
    peer: Address,
    config: Config,
    span_provider: SP,
}

type HyperRequest = http::request::Request<hyper::body::Incoming>;

impl<S, SP> hyper::service::Service<HyperRequest> for HyperService<S, SP>
where
    S: Service<ServerContext, Request> + Clone + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
    SP: SpanProvider + Clone + Send + Sync + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn call(&self, req: HyperRequest) -> Self::Future {
        let service = self.clone();
        Box::pin(
            METAINFO.scope(RefCell::new(MetaInfo::default()), async move {
                let mut cx = ServerContext::new(service.peer);
                cx.rpc_info_mut().set_config(service.config);
                let span = service.span_provider.on_serve(&cx);
                let resp = service
                    .inner
                    .call(&mut cx, req.map(Body::from_incoming))
                    .instrument(span)
                    .await
                    .into_response();
                service.span_provider.leave_serve(&cx);
                Ok(resp)
            }),
        )
    }
}
