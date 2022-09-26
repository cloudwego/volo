//! gRPC server for Volo.
//!
//! This module contains the low level component to build a gRPC server.

use std::{
    cell::RefCell, marker::PhantomData, net::SocketAddr, str::FromStr, sync::Arc, time::Duration,
};

use futures::{Future, TryStreamExt};
use http::{HeaderMap, HeaderValue};
use hyper::server::conn::Http;
use metainfo::{Backward, Forward};
use motore::{
    builder::ServiceBuilder,
    layer::{Identity, Layer, Stack},
    service::Service,
    BoxError, ServiceExt,
};
use tower::Layer as TowerLayer;
use volo::{
    context::{Context, Endpoint},
    net::Address,
    spawn,
};

use crate::{
    body::Body,
    codec::decode::Kind,
    context::ServerContext,
    message::{RecvEntryMessage, SendEntryMessage},
    metadata::{
        MetadataKey, MetadataMap, DESTINATION_SERVICE, HEADER_TRANS_REMOTE_ADDR, SOURCE_SERVICE,
    },
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
    pub fn http2_init_stream_window_size(&mut self, sz: impl Into<u32>) -> &mut Self {
        self.http2_config.init_stream_window_size = sz.into();
        self
    }

    /// Sets the max connection-level flow control for HTTP2.
    ///
    /// Default is `1MB`.
    pub fn http2_init_connection_window_size(&mut self, sz: impl Into<u32>) -> &mut Self {
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
    pub fn http2_max_concurrent_streams(&mut self, max: impl Into<Option<u32>>) -> &mut Self {
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
    pub fn http2_keepalive_interval(&mut self, interval: impl Into<Option<Duration>>) -> &mut Self {
        self.http2_config.http2_keepalive_interval = interval.into();
        self
    }

    /// Sets a timeout for receiving an acknowledgement of the keepalive ping.
    ///
    /// If the ping is not acknowledged within the timeout, the connection will be closed.
    /// Does nothing if http2_keepalive_interval is disabled.
    ///
    /// Default is 20 seconds.
    pub fn http2_keepalive_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.http2_config.http2_keepalive_timeout = timeout;
        self
    }

    /// Sets the maximum frame size to use for HTTP2.
    ///
    /// Passing `None` will do nothing.
    ///
    /// If not set, will default from underlying transport.
    pub fn http2_max_frame_size(&mut self, sz: impl Into<Option<u32>>) -> &mut Self {
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
    pub fn accept_http1(&mut self, accept_http1: bool) -> &mut Self {
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

        while let Some(conn) = incoming.try_next().await? {
            tracing::trace!("[VOLO] recv a connection from: {:?}", conn.info.peer_addr);
            let peer_addr = conn.info.peer_addr.clone();

            let service = HyperAdaptorLayer::new(peer_addr).layer(service.clone());
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

/// A layer that adapts a `motore::Service` to `tower::Service`.
pub struct HyperAdaptorLayer<T, U> {
    peer_addr: Option<Address>,
    _marker: PhantomData<(T, U)>,
}

impl<T, U> HyperAdaptorLayer<T, U> {
    pub fn new(peer_addr: Option<Address>) -> Self {
        Self {
            peer_addr,
            _marker: PhantomData,
        }
    }
}

impl<S, T, U> tower::Layer<S> for HyperAdaptorLayer<T, U> {
    type Service = HyperAdaptorService<S, T, U>;

    fn layer(&self, inner: S) -> Self::Service {
        HyperAdaptorService {
            inner,
            peer_addr: self.peer_addr.clone(),
            _marker: self._marker,
        }
    }
}

/// A service that implements `tower::Service` for service transition between hyper's
/// `tower::Service` and our's `motore::Service`. For more details, A incoming
/// request will first come to hyper's `tower::Service`, then `HyperAdaptorService`,
/// finally our's `motore::Service`.
#[derive(Clone)]
pub struct HyperAdaptorService<S, T, U> {
    inner: S,
    peer_addr: Option<Address>,
    _marker: PhantomData<(T, U)>,
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
        _: &mut ::core::task::Context<'_>,
    ) -> ::core::task::Poll<Result<(), Self::Error>> {
        ::core::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: hyper::Request<hyper::Body>) -> Self::Future {
        let mut inner = self.inner.clone();
        let peer_addr = self.peer_addr.clone();

        metainfo::METAINFO.scope(RefCell::new(metainfo::MetaInfo::default()), async move {
            let mut cx = ServerContext::default();
            cx.rpc_info.method = Some(req.uri().path().into());

            let (mut parts, body) = req.into_parts();

            status_to_http!(extract_metadata(&mut parts.headers, &mut cx, peer_addr));

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

            let (mut metadata, extensions, message) = volo_resp.into_parts();

            status_to_http!(insert_metadata(&mut metadata));

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

fn extract_metadata(
    headers: &mut HeaderMap<HeaderValue>,
    cx: &mut ServerContext,
    peer_addr: Option<Address>,
) -> Result<(), Status> {
    metainfo::METAINFO.with(|metainfo| {
        let mut metainfo = metainfo.borrow_mut();

        // caller
        if let Some(source_service) = headers.remove(SOURCE_SERVICE) {
            let source_service = Arc::<str>::from(source_service.to_str()?);
            let mut caller = Endpoint::new(source_service.into());
            if let Some(ad) = headers.remove(HEADER_TRANS_REMOTE_ADDR) {
                let addr = ad.to_str()?.parse::<SocketAddr>();
                if let Ok(addr) = addr {
                    caller.set_address(volo::net::Address::from(addr));
                }
            }
            if caller.address.is_none() {
                caller.address = peer_addr;
            }
            cx.rpc_info_mut().caller = Some(caller);
        }

        // callee
        if let Some(destination_service) = headers.remove(DESTINATION_SERVICE) {
            let destination_service = Arc::<str>::from(destination_service.to_str()?);
            let callee = Endpoint::new(destination_service.into());
            cx.rpc_info_mut().callee = Some(callee);
        }

        // persistent and transient
        let mut vec = Vec::with_capacity(headers.len());
        for (k, v) in headers.into_iter() {
            let k = k.as_str();
            let v = v.to_str()?;
            if k.starts_with(metainfo::HTTP_PREFIX_PERSISTENT) {
                vec.push(k.to_owned());
                metainfo.strip_http_prefix_and_set_persistent(k.to_owned(), v.to_owned());
            } else if k.starts_with(metainfo::HTTP_PREFIX_TRANSIENT) {
                vec.push(k.to_owned());
                metainfo.strip_http_prefix_and_set_upstream(k.to_owned(), v.to_owned());
            }
        }
        for k in vec {
            headers.remove(k);
        }

        Ok::<(), Status>(())
    })
}

fn insert_metadata(metadata: &mut MetadataMap) -> Result<(), Status> {
    metainfo::METAINFO.with(|metainfo| {
        let metainfo = metainfo.borrow_mut();

        // backward
        if let Some(at) = metainfo.get_all_backward_transients() {
            for (key, value) in at {
                let key = metainfo::HTTP_PREFIX_BACKWARD.to_owned() + key;
                metadata.insert(MetadataKey::from_str(key.as_str())?, value.parse()?);
            }
        }

        Ok::<(), Status>(())
    })
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
