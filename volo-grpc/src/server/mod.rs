//! gRPC server for Volo.
//!
//! This module contains the low level component to build a gRPC server.

mod incoming;
mod meta;
mod router;
mod service;

use std::{fmt, io, time::Duration};

use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use incoming::IncomingService;
pub use meta::MetaService;
use motore::{
    layer::{Identity, Layer, Stack},
    service::Service,
    BoxError,
};
pub use service::ServiceBuilder;
#[cfg(feature = "__tls")]
use volo::net::tls::{Acceptor, ServerTlsConfig};
use volo::{
    net::{conn::Conn, incoming::Incoming},
    spawn,
};

pub mod layer;
pub use self::router::Router;
use crate::{body::BoxBody, context::ServerContext, Request, Response, Status};

/// A trait to provide a static reference to the service's
/// name. This is used for routing service's within the router.
pub trait NamedService {
    /// The `Service-Name` as described [here].
    ///
    /// [here]: https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md#requests
    const NAME: &'static str;
}

/// A server for a gRPC service.
#[derive(Clone)]
pub struct Server<IL, OL> {
    inner_layer: IL,
    outer_layer: OL,
    http2_config: Http2Config,
    router: Router,

    #[cfg(feature = "__tls")]
    tls_config: Option<ServerTlsConfig>,
}

impl Default for Server<Identity, tower::layer::util::Identity> {
    fn default() -> Self {
        Self::new()
    }
}

impl Server<Identity, tower::layer::util::Identity> {
    /// Creates a new [`Server`].
    pub fn new() -> Self {
        Self {
            inner_layer: Identity::new(),
            outer_layer: tower::layer::util::Identity::new(),
            http2_config: Http2Config::default(),
            router: Router::new(),

            #[cfg(feature = "__tls")]
            tls_config: None,
        }
    }
}

impl<IL, OL> Server<IL, OL> {
    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    /// Sets the TLS configuration for the server.
    ///
    /// If not set, the server will not use TLS.
    pub fn tls_config(mut self, value: impl Into<ServerTlsConfig>) -> Self {
        self.tls_config = Some(value.into());
        self
    }

    /// Sets the `SETTINGS_INITIAL_WINDOW_SIZE` option for HTTP2
    /// stream-level flow control.
    ///
    /// Default is `1MB`.
    pub fn http2_init_stream_window_size(mut self, sz: impl Into<u32>) -> Self {
        self.http2_config.init_stream_window_size = sz.into();
        self
    }

    /// Sets the max connection-level flow control for HTTP2.
    ///
    /// Default is `1MB`.
    pub fn http2_init_connection_window_size(mut self, sz: impl Into<u32>) -> Self {
        self.http2_config.init_connection_window_size = sz.into();
        self
    }

    /// Sets whether to use an adaptive flow control.
    ///
    /// Enabling this will override the limits set in
    /// `http2_initial_stream_window_size` and
    /// `http2_initial_connection_window_size`.
    ///
    /// Default is `false`.
    pub fn http2_adaptive_window(mut self, enabled: bool) -> Self {
        self.http2_config.adaptive_window = enabled;
        self
    }

    /// Sets the `SETTINGS_MAX_CONCURRENT_STREAMS` option for HTTP2 connections.
    ///
    /// Default is no limit (`None`).
    pub fn http2_max_concurrent_streams(mut self, max: impl Into<Option<u32>>) -> Self {
        self.http2_config.max_concurrent_streams = max.into();
        self
    }

    /// Sets whether HTTP2 Ping frames are enabled on accepted connections.
    ///
    /// If `None` is specified, HTTP2 keepalive is disabled, otherwise the duration
    /// specified will be the time interval between HTTP2 Ping frames.
    /// The timeout for receiving an acknowledgement of the keepalive ping
    /// can be set with [`Server::http2_keepalive_timeout`].
    ///
    /// Default is no HTTP2 keepalive (`None`).
    pub fn http2_keepalive_interval(mut self, interval: impl Into<Option<Duration>>) -> Self {
        self.http2_config.http2_keepalive_interval = interval.into();
        self
    }

    /// Sets a timeout for receiving an acknowledgement of the keepalive ping.
    ///
    /// If the ping is not acknowledged within the timeout, the connection will be closed.
    /// Does nothing if http2_keepalive_interval is disabled.
    ///
    /// Default is 20 seconds.
    pub fn http2_keepalive_timeout(mut self, timeout: Duration) -> Self {
        self.http2_config.http2_keepalive_timeout = timeout;
        self
    }

    /// Sets the maximum frame size to use for HTTP2.
    ///
    /// Passing `None` will do nothing.
    ///
    /// If not set, will default from underlying transport.
    pub fn http2_max_frame_size(mut self, sz: impl Into<Option<u32>>) -> Self {
        self.http2_config.max_frame_size = sz.into();
        self
    }

    /// Set the maximum write buffer size for each HTTP/2 stream.
    ///
    /// Default is currently ~400KB, but may change.
    ///
    /// The value must be no larger than `u32::MAX`.
    pub fn http2_max_send_buf_size(mut self, max: impl Into<usize>) -> Self {
        self.http2_config.max_send_buf_size = max.into();
        self
    }

    /// Sets the max size of received header frames.
    ///
    /// Default is currently ~16MB, but may change.
    pub fn http2_max_header_list_size(mut self, max: impl Into<u32>) -> Self {
        self.http2_config.max_header_list_size = max.into();
        self
    }

    /// Allow this server to accept http1 requests.
    ///
    /// Accepting http1 requests is only useful when developing `grpc-web`
    /// enabled services. If this setting is set to `true` but services are
    /// not correctly configured to handle grpc-web requests, your server may
    /// return confusing (but correct) protocol errors.
    ///
    /// Default is `false`.
    pub fn accept_http1(mut self, accept_http1: bool) -> Self {
        self.http2_config.accept_http1 = accept_http1;
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
    ///
    /// The overall order for layers is: transport -> IncomingService -> outer -> MetaService ->
    /// \[inner\].
    pub fn layer<Inner>(self, layer: Inner) -> Server<Stack<Inner, IL>, OL> {
        Server {
            inner_layer: Stack::new(layer, self.inner_layer),
            outer_layer: self.outer_layer,
            http2_config: self.http2_config,
            router: self.router,
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
    ///
    /// The overall order for layers is: transport -> IncomingService -> outer -> MetaService ->
    /// \[inner\].
    pub fn layer_front<Front>(self, layer: Front) -> Server<Stack<IL, Front>, OL> {
        Server {
            inner_layer: Stack::new(self.inner_layer, layer),
            outer_layer: self.outer_layer,
            http2_config: self.http2_config,
            router: self.router,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Adds a new outer layer to the server.
    ///
    /// The layer's `Service` should be `Send + Sync + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer_tower(baz)`, we will get: foo -> bar -> baz.
    ///
    /// The overall order for layers is: transport -> IncomingService -> \[outer\] -> MetaService ->
    /// inner.
    pub fn layer_tower<Outer>(
        self,
        layer: Outer,
    ) -> Server<IL, tower::layer::util::Stack<Outer, OL>> {
        Server {
            inner_layer: self.inner_layer,
            outer_layer: tower::layer::util::Stack::new(layer, self.outer_layer),
            http2_config: self.http2_config,
            router: self.router,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Adds a new service to the router.
    pub fn add_service<S>(self, s: S) -> Self
    where
        S: Service<ServerContext, Request<BoxBody>, Response = Response<BoxBody>, Error = Status>
            + NamedService
            + Clone
            + Send
            + Sync
            + 'static,
    {
        Self {
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            http2_config: self.http2_config,
            router: self.router.add_service(s),
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// The main entry point for the server.
    /// Runs server with a stop signal to control graceful shutdown.
    pub async fn run_with_shutdown<
        A: volo::net::MakeIncoming,
        F: std::future::Future<Output = io::Result<()>>,
    >(
        self,
        incoming: A,
        signal: F,
    ) -> Result<(), BoxError>
    where
        IL: Layer<Router>,
        IL::Service: Service<ServerContext, Request<BoxBody>, Response = Response<BoxBody>>
            + Clone
            + Send
            + Sync
            + 'static,
        <IL::Service as Service<ServerContext, Request<BoxBody>>>::Error: Into<Status>,
        OL: tower::Layer<MetaService<IL::Service>>,
        OL::Service: tower::Service<hyper::Request<BoxBody>, Response = hyper::Response<BoxBody>>
            + Clone
            + Send
            + 'static,
        <OL::Service as tower::Service<hyper::Request<BoxBody>>>::Future: Send + 'static,
        <OL::Service as tower::Service<hyper::Request<BoxBody>>>::Error:
            Into<Status> + Send + Sync + std::error::Error,
    {
        let mut incoming = incoming.make_incoming().await?;
        tracing::info!("[VOLO] server start at: {:?}", incoming);

        let service = self
            .outer_layer
            .layer(MetaService::new(self.inner_layer.layer(self.router)));

        tokio::pin!(signal);
        let (tx, rx) = tokio::sync::watch::channel(());

        loop {
            tokio::select! {
                _ = &mut signal => {
                    drop(rx);
                    tracing::info!("[VOLO] graceful shutdown");
                    let _ = tx.send(());
                    // Waits for receivers to drop.
                    tx.closed().await;
                    return Ok(());
                },
                conn = incoming.accept() => {
                    let conn: Conn = match conn? {
                        Some(c) => c,
                        None => return Ok(()),
                    };
                    #[cfg(feature = "__tls")]
                    let conn = {
                        let Conn {
                            stream,
                            info,
                        } = conn;
                        // Only perform TLS handshake if either rustls or native-tls is configured
                        match (stream, self.tls_config.as_ref().map(|o| &o.acceptor)) {
                            (volo::net::conn::ConnStream::Tcp(tcp), Some(tls_acceptor)) => {
                                let stream = match tls_acceptor.accept(tcp).await {
                                    Ok(stream) => stream,
                                    Err(err) => {
                                        tracing::debug!("[VOLO] TLS handshake error: {:?}", err);
                                        continue;
                                    },
                                };
                                Conn {
                                    stream,
                                    info,
                                }
                            },
                            (stream, _) => Conn {
                                stream,
                                info
                            },
                        }
                    };

                    tracing::trace!("[VOLO] recv a connection from: {:?}", conn.info.peer_addr);
                    let peer_addr = conn.info.peer_addr.clone();

                    let service = IncomingService::new(service.clone(), peer_addr);

                    // init server
                    let mut server = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                    if !self.http2_config.accept_http1 {
                        server = server.http2_only();
                    }
                    server
                        .http2()
                        .initial_stream_window_size(self.http2_config.init_stream_window_size)
                        .timer(TokioTimer::new())
                        .initial_connection_window_size(self.http2_config.init_connection_window_size)
                        .adaptive_window(self.http2_config.adaptive_window)
                        .max_concurrent_streams(self.http2_config.max_concurrent_streams)
                        .keep_alive_interval(self.http2_config.http2_keepalive_interval)
                        .keep_alive_timeout(self.http2_config.http2_keepalive_timeout)
                        .max_frame_size(self.http2_config.max_frame_size)
                        .max_send_buf_size(self.http2_config.max_send_buf_size)
                        .max_header_list_size(self.http2_config.max_header_list_size);

                    let mut watch = rx.clone();
                    spawn(async move {
                        let mut http_conn = std::pin::pin!(server.serve_connection(
                            TokioIo::new(conn),
                            hyper::service::service_fn(move |req| {
                                let mut service = service.clone();
                                async move {
                                    tower::Service::call(&mut service, req).await
                                }
                            })
                        ));
                        loop {
                            tokio::select! {
                                _ = watch.changed() => {
                                    tracing::trace!("[VOLO] closing a pending connection");
                                    // Graceful shutdown.
                                    http_conn.as_mut().graceful_shutdown();
                                },
                                result = &mut http_conn => {
                                    if let Err(err) = result {
                                        tracing::debug!("[VOLO] connection error: {:?}", err);
                                    }
                                    break;
                                },
                            }
                        }
                    });
                },
            }
        }
    }

    /// The main entry point for the server.
    pub async fn run<A: volo::net::MakeIncoming>(self, incoming: A) -> Result<(), BoxError>
    where
        IL: Layer<Router>,
        IL::Service: Service<ServerContext, Request<BoxBody>, Response = Response<BoxBody>>
            + Clone
            + Send
            + Sync
            + 'static,
        <IL::Service as Service<ServerContext, Request<BoxBody>>>::Error: Into<Status>,
        OL: tower::Layer<MetaService<IL::Service>>,
        OL::Service: tower::Service<hyper::Request<BoxBody>, Response = hyper::Response<BoxBody>>
            + Clone
            + Send
            + 'static,
        <OL::Service as tower::Service<hyper::Request<BoxBody>>>::Future: Send + 'static,
        <OL::Service as tower::Service<hyper::Request<BoxBody>>>::Error:
            Into<Status> + Send + Sync + std::error::Error,
    {
        self.run_with_shutdown(incoming, tokio::signal::ctrl_c())
            .await
    }
}

impl<IL, OL> fmt::Debug for Server<IL, OL> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("http2_config", &self.http2_config)
            .field("router", &self.router)
            .finish()
    }
}

const DEFAULT_KEEPALIVE_TIMEOUT_SECS: Duration = Duration::from_secs(20);
const DEFAULT_CONN_WINDOW_SIZE: u32 = 1024 * 1024; // 1MB
const DEFAULT_STREAM_WINDOW_SIZE: u32 = 1024 * 1024; // 1MB
const DEFAULT_MAX_SEND_BUF_SIZE: usize = 1024 * 400; // 400kb
const DEFAULT_SETTINGS_MAX_HEADER_LIST_SIZE: u32 = 16 << 20; // 16 MB "sane default" taken from golang http2

/// Configuration for the underlying h2 connection.
#[derive(Debug, Clone, Copy)]
pub struct Http2Config {
    pub(crate) init_stream_window_size: u32,
    pub(crate) init_connection_window_size: u32,
    pub(crate) max_concurrent_streams: Option<u32>,
    pub(crate) adaptive_window: bool,
    pub(crate) http2_keepalive_interval: Option<Duration>,
    pub(crate) http2_keepalive_timeout: Duration,
    pub(crate) max_frame_size: Option<u32>,
    pub(crate) max_send_buf_size: usize,
    pub(crate) max_header_list_size: u32,
    pub(crate) accept_http1: bool,
}

impl Default for Http2Config {
    fn default() -> Self {
        Self {
            init_stream_window_size: DEFAULT_STREAM_WINDOW_SIZE,
            init_connection_window_size: DEFAULT_CONN_WINDOW_SIZE,
            adaptive_window: false,
            max_concurrent_streams: None,
            http2_keepalive_interval: None,
            http2_keepalive_timeout: DEFAULT_KEEPALIVE_TIMEOUT_SECS,
            max_frame_size: None,
            max_send_buf_size: DEFAULT_MAX_SEND_BUF_SIZE,
            max_header_list_size: DEFAULT_SETTINGS_MAX_HEADER_LIST_SIZE,
            accept_http1: false,
        }
    }
}
