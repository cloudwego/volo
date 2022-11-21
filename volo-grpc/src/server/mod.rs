//! gRPC server for Volo.
//!
//! This module contains the low level component to build a gRPC server.

mod meta;

use std::{cell::RefCell, marker::PhantomData, time::Duration};

use futures::Future;
use hyper::server::conn::Http;
use motore::{
    builder::ServiceBuilder,
    layer::{Identity, Layer, Stack},
    service::Service,
    BoxError, ServiceExt,
};
use volo::{net::incoming::Incoming, spawn};

use crate::{
    body::Body,
    codec::decode::Kind,
    context::ServerContext,
    message::{RecvEntryMessage, SendEntryMessage},
    server::meta::MetaService,
    Request, Response, Status,
};

/// A server for a gRPC service.
pub struct Server<S, L> {
    service: S,
    layer: L,
    http2_config: Http2Config,
}

impl<S> Server<S, Identity> {
    /// Creates a new [`Server`].
    pub fn new(service: S) -> Self {
        Self {
            service,
            layer: Identity::new(),
            http2_config: Http2Config::default(),
        }
    }
}

impl<S, L> Server<S, L> {
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
    /// The layer's `Service` should be `Send + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer(baz)`, we will get: foo -> bar -> baz.
    pub fn layer<O>(self, layer: O) -> Server<S, Stack<O, L>> {
        Server {
            layer: Stack::new(layer, self.layer),
            service: self.service,
            http2_config: self.http2_config,
        }
    }

    /// Adds a new front layer to the server.
    ///
    /// The layer's `Service` should be `Send + Clone + 'static`.
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
            layer: Stack::new(self.layer, layer),
            service: self.service,
            http2_config: self.http2_config,
        }
    }

    /// The main entry point for the server.
    pub async fn run<A: volo::net::MakeIncoming, T, U>(self, incoming: A) -> Result<(), BoxError>
    where
        L: Layer<S>,
        L::Service:
            Service<ServerContext, Request<T>, Response = Response<U>> + Clone + Send + 'static,
        <L::Service as Service<ServerContext, Request<T>>>::Error: Into<Status> + Send,
        S: Service<ServerContext, Request<T>, Response = Response<U>, Error = Status>
            + Send
            + Clone
            + 'static,
        T: Send + 'static + RecvEntryMessage,
        U: Send + 'static + SendEntryMessage,
    {
        let mut incoming = incoming.make_incoming().await?;
        tracing::info!("[VOLO] server start at: {:?}", incoming);

        let service = ServiceBuilder::new()
            .layer(self.layer)
            .service(self.service);
        let service = service.map_err(|err| err.into());

        while let Some(conn) = incoming.accept().await? {
            tracing::trace!("[VOLO] recv a connection from: {:?}", conn.info.peer_addr);
            let peer_addr = conn.info.peer_addr.clone();

            let service = HyperAdaptorService::new(MetaService::new(service.clone(), peer_addr));
            // init server
            let server = Self::create_http_server(&self.http2_config);
            spawn(async move {
                let result = server.serve_connection(conn, service).await;
                if let Err(err) = result {
                    tracing::warn!("[VOLO] http server fail to serve: {:?}", err);
                }
            });
        }
        Ok(())
    }

    fn create_http_server(http2_config: &Http2Config) -> Http {
        let mut server = Http::new();
        server
            .http2_only(!http2_config.accept_http1)
            .http2_initial_stream_window_size(http2_config.init_stream_window_size)
            .http2_initial_connection_window_size(http2_config.init_connection_window_size)
            .http2_adaptive_window(http2_config.adaptive_window)
            .http2_max_concurrent_streams(http2_config.max_concurrent_streams)
            .http2_keep_alive_interval(http2_config.http2_keepalive_interval)
            .http2_keep_alive_timeout(http2_config.http2_keepalive_timeout)
            .http2_max_frame_size(http2_config.max_frame_size);
        server
    }
}

macro_rules! status_to_http {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(status) => return Ok(status.to_http()),
        }
    };
}

/// A service that implements `tower::Service` for service transition between hyper's
/// `tower::Service` and our's `motore::Service`. For more details, A incoming
/// request will first come to hyper's `tower::Service`, then `HyperAdaptorService`,
/// finally our's `motore::Service`.
#[derive(Clone)]
pub struct HyperAdaptorService<S, T, U> {
    inner: S,
    _marker: PhantomData<(T, U)>,
}

impl<S, T, U> HyperAdaptorService<S, T, U> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            _marker: PhantomData,
        }
    }
}

impl<S, T, U> tower::Service<hyper::Request<hyper::Body>> for HyperAdaptorService<S, T, U>
where
    S: Service<ServerContext, Request<T>, Response = Response<U>> + Clone + Send + 'static,
    S::Error: Into<Status>,
    T: RecvEntryMessage,
    U: SendEntryMessage,
{
    type Response = hyper::Response<Body>;
    type Error = Status;
    type Future = impl Future<Output = Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Result<(), Self::Error>> {
        core::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: hyper::Request<hyper::Body>) -> Self::Future {
        let mut inner = self.inner.clone();

        metainfo::METAINFO.scope(RefCell::new(metainfo::MetaInfo::default()), async move {
            let mut cx = ServerContext::default();
            cx.rpc_info.method = Some(req.uri().path().into());

            let (parts, body) = req.into_parts();

            let message = status_to_http!(T::from_body(
                cx.rpc_info.method.as_deref(),
                body,
                Kind::Request
            ));

            let volo_req = Request::from_http_parts(parts, message);

            let volo_resp = match inner.call(&mut cx, volo_req).await {
                Ok(resp) => resp,
                Err(err) => {
                    return Ok(err.into().to_http());
                }
            };

            let (metadata, extensions, message) = volo_resp.into_parts();

            let mut resp = hyper::Response::new(Body::new(message.into_body()));
            *resp.headers_mut() = metadata.into_headers();
            *resp.extensions_mut() = extensions;
            resp.headers_mut().insert(
                http::header::CONTENT_TYPE,
                http::header::HeaderValue::from_static("application/grpc"),
            );

            Ok(resp)
        })
    }
}

const DEFAULT_KEEPALIVE_TIMEOUT_SECS: Duration = Duration::from_secs(20);
const DEFAULT_CONN_WINDOW_SIZE: u32 = 1024 * 1024; // 1MB
const DEFAULT_STREAM_WINDOW_SIZE: u32 = 1024 * 1024; // 1MB

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
            accept_http1: false,
        }
    }
}
