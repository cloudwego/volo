//! gRPC server for Volo.
//!
//! This module contains the low level component to build a gRPC server.

mod meta;
mod router;
mod service;

use std::{fmt, io, time::Duration};

use motore::{
    layer::{Identity, Layer, Stack},
    service::{Service, TowerAdapter},
    BoxError,
};
pub use service::ServiceBuilder;
use volo::{
    net::{
        conn::{Conn, ConnStream},
        incoming::Incoming,
    },
    spawn,
};

pub use self::router::Router;
use crate::{
    body::Body,
    context::ServerContext,
    server::meta::MetaService,
    transport::tls::{ServerTlsConfig, TlsAcceptor},
    Request, Response, Status,
};

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
pub struct Server<L> {
    layer: L,
    http2_config: Http2Config,
    router: Router,

    #[cfg(any(feature = "rustls", feature = "native-tls"))]
    tls_config: Option<ServerTlsConfig>,
}

impl Default for Server<Identity> {
    fn default() -> Self {
        Self::new()
    }
}

impl Server<Identity> {
    /// Creates a new [`Server`].
    pub fn new() -> Self {
        Self {
            layer: Identity::new(),
            http2_config: Http2Config::default(),
            router: Router::new(),

            #[cfg(any(feature = "rustls", feature = "native-tls"))]
            tls_config: None,
        }
    }
}

impl<L> Server<L> {
    cfg_rustls_or_native_tls! {
        /// Sets the TLS configuration for the server.
        ///
        /// If not set, the server will not use TLS.
        pub fn tls_config(mut self, value: impl Into<ServerTlsConfig>) -> Self {
            self.tls_config = Some(value.into());
            self
        }
    }

    /// Sets the [`SETTINGS_INITIAL_WINDOW_SIZE`] option for HTTP2
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

    /// Sets the [`SETTINGS_MAX_CONCURRENT_STREAMS`] option for HTTP2 connections.
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
    pub fn layer<O>(self, layer: O) -> Server<Stack<O, L>> {
        Server {
            layer: Stack::new(layer, self.layer),
            http2_config: self.http2_config,
            router: self.router,
            #[cfg(any(feature = "rustls", feature = "native-tls"))]
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
    pub fn layer_front<Front>(self, layer: Front) -> Server<Stack<L, Front>> {
        Server {
            layer: Stack::new(self.layer, layer),
            http2_config: self.http2_config,
            router: self.router,
            #[cfg(any(feature = "rustls", feature = "native-tls"))]
            tls_config: self.tls_config,
        }
    }

    /// Adds a new service to the router.
    pub fn add_service<S>(self, s: S) -> Self
    where
        S: Service<ServerContext, Request<hyper::Body>, Response = Response<Body>, Error = Status>
            + NamedService
            + Clone
            + Send
            + Sync
            + 'static,
    {
        Self {
            layer: self.layer,
            http2_config: self.http2_config,
            router: self.router.add_service(s),
            #[cfg(any(feature = "rustls", feature = "native-tls"))]
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
        L: Layer<Router>,
        L::Service: Service<ServerContext, Request<hyper::Body>, Response = Response<Body>>
            + Clone
            + Send
            + Sync
            + 'static,
        <L::Service as Service<ServerContext, Request<hyper::Body>>>::Error: Into<Status> + Send,
    {
        let mut incoming = incoming.make_incoming().await?;
        tracing::info!("[VOLO] server start at: {:?}", incoming);

        let service = motore::builder::ServiceBuilder::new()
            .layer(self.layer)
            .service(self.router);

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
                    let info = conn.info;
                    // Only perform TLS handshake if either rustls or native-tls is configured
                    let conn: Conn = match (conn.stream, self.tls_config.as_ref().map(|o| &o.acceptor)) {
                        (volo::net::conn::ConnStream::Tcp(tcp), Some(TlsAcceptor::Rustls(tls_acceptor))) => {
                            let stream = tls_acceptor.accept(tcp).await?;
                            Conn {
                                stream: ConnStream::Rustls(tokio_rustls::TlsStream::Server(stream)),
                                info
                            }
                        },
                        (volo::net::conn::ConnStream::Tcp(tcp), Some(TlsAcceptor::NativeTls(tls_acceptor))) => {
                            let stream = tls_acceptor.accept(tcp).await?;
                            Conn {
                                stream: ConnStream::NativeTls(stream),
                                info,
                            }
                        },
                        (stream, _) => Conn {
                            stream,
                            info
                        },
                    };

                    tracing::trace!("[VOLO] recv a connection from: {:?}", conn.info.peer_addr);
                    let peer_addr = conn.info.peer_addr.clone();

                    let service = MetaService::new(service.clone(), peer_addr)
                        .tower(|req| (ServerContext::default(), req));

                    // init server
                    let mut server = hyper::server::conn::Http::new();
                    server
                        .http2_only(!self.http2_config.accept_http1)
                        .http2_initial_stream_window_size(self.http2_config.init_stream_window_size)
                        .http2_initial_connection_window_size(self.http2_config.init_connection_window_size)
                        .http2_adaptive_window(self.http2_config.adaptive_window)
                        .http2_max_concurrent_streams(self.http2_config.max_concurrent_streams)
                        .http2_keep_alive_interval(self.http2_config.http2_keepalive_interval)
                        .http2_keep_alive_timeout(self.http2_config.http2_keepalive_timeout)
                        .http2_max_frame_size(self.http2_config.max_frame_size)
                        .http2_max_send_buf_size(self.http2_config.max_send_buf_size)
                        .http2_max_header_list_size(self.http2_config.max_header_list_size);

                    let mut watch = rx.clone();
                    spawn(async move {
                        let mut http_conn = server.serve_connection(conn, service);
                        tokio::select! {
                            _ = watch.changed() => {
                                tracing::trace!("[VOLO] closing a pending connection");
                                // Graceful shutdown.
                                hyper::server::conn::Connection::graceful_shutdown(Pin::new(&mut http_conn));
                                // Continue to poll this connection until shutdown can finish.
                                let result = http_conn.await;
                                if let Err(err) = result {
                                    tracing::debug!("[VOLO] connection error: {:?}", err);
                                }
                            },
                            result = &mut http_conn => {
                                if let Err(err) = result {
                                    tracing::debug!("[VOLO] connection error: {:?}", err);
                                }
                            },
                        }
                    });
                },
            }
        }
    }

    /// The main entry point for the server.
    pub async fn run<A: volo::net::MakeIncoming>(self, incoming: A) -> Result<(), BoxError>
    where
        L: Layer<Router>,
        L::Service: Service<ServerContext, Request<hyper::Body>, Response = Response<Body>>
            + Clone
            + Send
            + Sync
            + 'static,
        <L::Service as Service<ServerContext, Request<hyper::Body>>>::Error: Into<Status> + Send,
    {
        self.run_with_shutdown(incoming, tokio::signal::ctrl_c())
            .await
    }
}

impl<L> fmt::Debug for Server<L> {
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
