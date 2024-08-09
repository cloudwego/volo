//! Server implementation
//!
//! See [`Server`] for more details.

use std::{
    cell::RefCell,
    convert::Infallible,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::future::BoxFuture;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use metainfo::{MetaInfo, METAINFO};
use motore::{
    layer::{Identity, Layer, Stack},
    service::Service,
    BoxError,
};
use parking_lot::RwLock;
use scopeguard::defer;
use tokio::sync::Notify;
#[cfg(feature = "__tls")]
use volo::net::{conn::ConnStream, tls::Acceptor, tls::ServerTlsConfig};
use volo::{
    context::Context,
    net::{conn::Conn, incoming::Incoming, Address, MakeIncoming},
};

use crate::{
    body::Body,
    context::{server::Config, ServerContext},
    request::ServerRequest,
    response::ServerResponse,
};

pub mod extract;
mod handler;
pub mod layer;
pub mod middleware;
pub mod panic_handler;
pub mod param;
pub mod response;
pub mod route;
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

    pub use super::{param::PathParams, route::Router, Server};
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
///     route::{get, Router},
///     Server,
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
pub struct Server<S, L> {
    service: S,
    layer: L,
    server: http1::Builder,
    config: Config,
    shutdown_hooks: Vec<Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>>,
    #[cfg(feature = "__tls")]
    tls_config: Option<ServerTlsConfig>,
}

impl<S> Server<S, Identity> {
    /// Create a new server.
    pub fn new(service: S) -> Self {
        Self {
            service,
            layer: Identity::new(),
            server: http1::Builder::new(),
            config: Config::default(),
            shutdown_hooks: Vec::new(),
            #[cfg(feature = "__tls")]
            tls_config: None,
        }
    }
}

impl<S, L> Server<S, L> {
    /// Enable TLS with the specified configuration.
    ///
    /// If not set, the server will not use TLS.
    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    pub fn tls_config(mut self, config: impl Into<ServerTlsConfig>) -> Self {
        self.tls_config = Some(config.into());
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
    pub fn layer<Inner>(self, layer: Inner) -> Server<S, Stack<Inner, L>> {
        Server {
            service: self.service,
            layer: Stack::new(layer, self.layer),
            server: self.server,
            config: self.config,
            shutdown_hooks: self.shutdown_hooks,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
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
            server: self.server,
            config: self.config,
            shutdown_hooks: self.shutdown_hooks,
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

    /// Set whether HTTP/1 connections should support half-closures.
    ///
    /// Clients can chose to shutdown their write-side while waiting
    /// for the server to respond. Setting this to `true` will
    /// prevent closing the connection immediately if `read`
    /// detects an EOF in the middle of a request.
    ///
    /// Default is `false`.
    pub fn set_half_close(&mut self, half_close: bool) -> &mut Self {
        self.server.half_close(half_close);
        self
    }

    /// Enables or disables HTTP/1 keep-alive.
    ///
    /// Default is true.
    pub fn set_keep_alive(&mut self, keep_alive: bool) -> &mut Self {
        self.server.keep_alive(keep_alive);
        self
    }

    /// Set whether HTTP/1 connections will write header names as title case at
    /// the socket level.
    ///
    /// Default is false.
    pub fn set_title_case_headers(&mut self, title_case_headers: bool) -> &mut Self {
        self.server.title_case_headers(title_case_headers);
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
        self.server.preserve_header_case(preserve_header_case);
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
        self.server.max_headers(max_headers);
        self
    }

    /// The main entry point for the server.
    pub async fn run<MI, B, E>(self, mk_incoming: MI) -> Result<(), BoxError>
    where
        S: Service<ServerContext, ServerRequest<B>, Error = E> + Send + Sync + 'static,
        S::Response: IntoResponse,
        E: IntoResponse,
        L: Layer<S> + Send + Sync + 'static,
        L::Service:
            Service<ServerContext, ServerRequest, Error = Infallible> + Send + Sync + 'static,
        <L::Service as Service<ServerContext, ServerRequest>>::Response: IntoResponse,
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
async fn serve<I, S, E>(
    server: Arc<http1::Builder>,
    mut incoming: I,
    service: S,
    config: Config,
    exit_flag: Arc<RwLock<bool>>,
    conn_cnt: Arc<AtomicUsize>,
    exit_notify: Arc<Notify>,
    #[cfg(feature = "__tls")] tls_config: Option<ServerTlsConfig>,
) where
    I: Incoming,
    S: Service<ServerContext, ServerRequest, Error = E> + Clone + Send + Sync + 'static,
    S::Response: IntoResponse,
    E: IntoResponse,
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
    server: Arc<http1::Builder>,
    conn: Conn,
    service: S,
    conn_cnt: Arc<AtomicUsize>,
    exit_notify: Arc<Notify>,
) where
    S: hyper::service::HttpService<hyper::body::Incoming, ResBody = Body>,
{
    conn_cnt.fetch_add(1, Ordering::Relaxed);
    defer! {
        conn_cnt.fetch_sub(1, Ordering::Relaxed);
    }

    let notified = exit_notify.notified();
    tokio::pin!(notified);

    let mut http_conn = server
        .serve_connection(TokioIo::new(conn), service)
        .with_upgrades();

    tokio::select! {
        _ = &mut notified => {
            tracing::trace!("[Volo-HTTP] closing a pending connection");
            // Graceful shutdown.
            hyper::server::conn::http1::UpgradeableConnection::graceful_shutdown(
                Pin::new(&mut http_conn)
            );
            // Continue to poll this connection until shutdown can finish.
            let result = http_conn.await;
            if let Err(err) = result {
                tracing::debug!("[Volo-HTTP] connection error: {:?}", err);
            }
        }
        result = &mut http_conn => {
            if let Err(err) = result {
                tracing::debug!("[Volo-HTTP] connection error: {:?}", err);
            }
        },
    }
}

#[derive(Clone)]
struct HyperService<S> {
    inner: S,
    peer: Address,
    config: Config,
}

impl<S, E> hyper::service::Service<ServerRequest> for HyperService<S>
where
    S: Service<ServerContext, ServerRequest, Error = E> + Clone + Send + Sync + 'static,
    S::Response: IntoResponse,
    E: IntoResponse,
{
    type Response = ServerResponse;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn call(&self, req: ServerRequest) -> Self::Future {
        let service = self.clone();
        Box::pin(
            METAINFO.scope(RefCell::new(MetaInfo::default()), async move {
                let mut cx = ServerContext::new(service.peer);
                cx.rpc_info_mut().set_config(service.config);
                Ok(service.inner.call(&mut cx, req).await.into_response())
            }),
        )
    }
}
