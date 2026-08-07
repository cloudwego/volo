//! Protocol related implementations

use std::{
    error::Error,
    future::Future,
    pin::Pin,
    str::FromStr,
    sync::LazyLock,
    task::{Context as TaskContext, Poll},
};

use futures::{
    FutureExt, TryFutureExt,
    future::{self, Either},
};
use http::{
    header,
    uri::{Authority, Scheme, Uri},
    version::Version,
};
use hyper::{body::Incoming, client::conn};
use hyper_util::rt::TokioIo;
use motore::{make::MakeConnection, service::Service};
use pin_project::pin_project;
use volo::{context::Context, net::Address};

use super::{
    connector::{HttpMakeConnection, PeerInfo},
    pool::{self, Connecting, Pool, Poolable, Pooled, Reservation},
};
use crate::{
    body::Body,
    context::ClientContext,
    error::{
        BoxError, ClientError,
        client::{Result, connect_error, no_address, request_error, retry, tri},
    },
    request::Request,
    response::Response,
    utils::lazy::Started,
};

/// Configuration of HTTP/1
#[derive(Default)]
pub(crate) struct ClientConfig {
    #[cfg(feature = "http1")]
    pub h1: super::http1::Config,
    #[cfg(feature = "http2")]
    pub h2: super::http2::Config,
}

#[derive(Clone)]
pub(crate) struct ClientTransportConfig {
    pub stat_enable: bool,
    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    pub disable_tls: bool,
}

impl Default for ClientTransportConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientTransportConfig {
    pub fn new() -> Self {
        Self {
            stat_enable: true,
            #[cfg(feature = "__tls")]
            disable_tls: false,
        }
    }
}

/// Transport service of HTTP Client.
///
/// This service will connect to the [`Address`] of callee's [`Endpoint`] in [`ClientContext`], then
/// send a [`Request`] to the destination server, and return a [`Response`] the server response.
///
/// [`Endpoint`]: volo::context::Endpoint
/// [`Request`]: http::request::Request
/// [`Response`]: http::response::Response
pub struct ClientTransport<B = Body> {
    #[cfg(feature = "http1")]
    h1_client: conn::http1::Builder,
    #[cfg(feature = "http2")]
    h2_client: conn::http2::Builder<hyper_util::rt::TokioExecutor>,
    config: ClientTransportConfig,
    connector: HttpMakeConnection,
    pool: Pool<PoolKey, HttpConnection<B>>,
    idle_pool_enabled: bool,
}

type PoolKey = (Scheme, Address);

impl<B> ClientTransport<B> {
    pub(crate) fn new(
        http_config: ClientConfig,
        transport_config: ClientTransportConfig,
        pool_config: pool::Config,
        #[cfg(feature = "__tls")] tls_connector: Option<volo::net::tls::TlsConnector>,
    ) -> Self {
        #[cfg(feature = "http1")]
        let h1_client = super::http1::client(&http_config.h1);
        #[cfg(feature = "http2")]
        let h2_client = super::http2::client(&http_config.h2);

        let builder = HttpMakeConnection::builder(&transport_config);
        #[cfg(feature = "__tls")]
        let builder = match tls_connector {
            Some(connector) => builder.with_tls_connector(connector),
            None => builder,
        };
        let connector = builder.build();

        Self {
            #[cfg(feature = "http1")]
            h1_client,
            #[cfg(feature = "http2")]
            h2_client,
            config: transport_config,
            connector,
            pool: Pool::new(pool_config),
            idle_pool_enabled: pool_config.max_idle_per_host > 0,
        }
    }

    fn connect_to(
        &self,
        ver: pool::Ver,
        peer: PeerInfo,
    ) -> impl Started<Output = Result<Pooled<PoolKey, HttpConnection<B>>>> + Send + 'static
    where
        B: http_body::Body + Unpin + Send + 'static,
        B::Data: Send,
        B::Error: Into<BoxError> + 'static,
    {
        let key = (peer.scheme.clone(), peer.address.clone());
        let connector = self.connector.clone();
        let pool = self.pool.clone();
        #[cfg(feature = "http1")]
        let h1_client = self.h1_client.clone();
        #[cfg(feature = "http2")]
        let h2_client = self.h2_client.clone();

        crate::utils::lazy::lazy(move || {
            let connecting = match pool.connecting(&key, ver) {
                Some(lock) => lock,
                None => return Either::Right(future::err(retry())),
            };
            Either::Left(Box::pin(connect_impl(
                ver,
                peer,
                connector,
                pool,
                connecting,
                #[cfg(feature = "http1")]
                h1_client,
                #[cfg(feature = "http2")]
                h2_client,
            )))
        })
    }

    async fn pooled_connect(
        &self,
        ver: Version,
        peer: PeerInfo,
    ) -> Result<Pooled<PoolKey, HttpConnection<B>>>
    where
        B: http_body::Body + Unpin + Send + 'static,
        B::Data: Send,
        B::Error: Into<BoxError> + 'static,
    {
        let key = (peer.scheme.clone(), peer.address.clone());

        let checkout = self.pool.checkout(key);
        let connect = self.connect_to(ver.into(), peer);

        // Well, `futures::future::select` is more suitable than `tokio::select!` in this case.
        match future::select(checkout, connect).await {
            Either::Left((Ok(checked_out), connecting)) => {
                // Checkout is done while connecting is started
                if connecting.started() {
                    let conn_fut = connecting
                        .map_err(|err| tracing::trace!("background connect error: {err}"))
                        .map(|_pooled| {
                            // Drop the `Pooled` and put it into pool in `Drop`
                        });
                    // Spawn it for finishing the connecting
                    tokio::spawn(conn_fut);
                }
                Ok(checked_out)
            }
            Either::Right((Ok(connected), _checkout)) => Ok(connected),
            Either::Left((Err(err), connecting)) => {
                // The checked out connection was closed, just continue the connecting
                if err.is_canceled() {
                    connecting.await
                } else {
                    // unreachable?
                    Err(connect_error(err))
                }
            }
            Either::Right((Err(err), checkout)) => {
                // The connection failed while acquiring the pool lock, and we should retry the
                // checkout.
                if err
                    .source()
                    .is_some_and(<dyn Error>::is::<crate::error::client::Retry>)
                {
                    checkout.await.map_err(connect_error)
                } else {
                    // Unexpected connect error
                    Err(err)
                }
            }
        }
    }
}

async fn connect_impl<B>(
    _ver: pool::Ver,
    peer: PeerInfo,
    connector: HttpMakeConnection,
    pool: Pool<PoolKey, HttpConnection<B>>,
    connecting: Connecting<PoolKey, HttpConnection<B>>,
    #[cfg(feature = "http1")] h1_client: conn::http1::Builder,
    #[cfg(feature = "http2")] h2_client: conn::http2::Builder<hyper_util::rt::TokioExecutor>,
) -> Result<Pooled<PoolKey, HttpConnection<B>>>
where
    B: http_body::Body + Unpin + Send + 'static,
    B::Data: Send,
    B::Error: Into<BoxError> + 'static,
{
    let conn = match connector.make_connection(peer).await {
        Ok(conn) => conn,
        Err(err) => {
            tracing::warn!("[Volo-HTTP] failed to make connection: {err}");
            return Err(err);
        }
    };

    #[cfg(feature = "http2")]
    let use_h2 = conn_use_h2(_ver, &conn);
    #[cfg(not(feature = "http2"))]
    let use_h2 = false;

    let conn = TokioIo::new(conn);
    if use_h2 {
        #[cfg(feature = "http2")]
        {
            let connecting = if _ver == pool::Ver::Auto {
                tri!(connecting.alpn_h2(&pool).ok_or_else(retry))
            } else {
                connecting
            };
            let (mut sender, conn) = tri!(h2_client.handshake(conn).await.map_err(connect_error));
            tokio::spawn(conn);
            // Wait for `conn` to ready up before we declare self sender as usable.
            tri!(sender.ready().await.map_err(connect_error));
            Ok(pool.pooled(connecting, HttpConnection::H2(sender)))
        }
        #[cfg(not(feature = "http2"))]
        Err(crate::error::client::bad_version())
    } else {
        #[cfg(feature = "http1")]
        {
            let (mut sender, conn) = tri!(h1_client.handshake(conn).await.map_err(connect_error));
            tokio::spawn(conn);
            // Wait for `conn` to ready up before we declare self sender as usable.
            tri!(sender.ready().await.map_err(connect_error));
            Ok(pool.pooled(connecting, HttpConnection::H1(sender)))
        }
        #[cfg(not(feature = "http1"))]
        Err(crate::error::client::bad_version())
    }
}

#[cfg(feature = "http2")]
fn conn_use_h2(ver: pool::Ver, _conn: &volo::net::conn::Conn) -> bool {
    #[cfg(feature = "__tls")]
    let use_h2 = match _conn.stream.negotiated_alpn().as_deref() {
        Some(alpn) => {
            // ALPN negotiated to use H2
            if alpn == b"h2" {
                return true;
            }
            // ALPN negotiated not to use H2
            false
        }
        // Use H2 by default
        None => true,
    };
    #[cfg(not(feature = "__tls"))]
    let use_h2 = true;

    // H2 is specified or H1 is disabled
    if use_h2 && (ver == pool::Ver::Http2 || cfg!(not(feature = "http1"))) {
        return true;
    }

    false
}

impl<B> Service<ClientContext, Request<B>> for ClientTransport<B>
where
    B: http_body::Body + Unpin + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn Error + Send + Sync>> + 'static,
{
    type Response = Response;
    type Error = ClientError;

    async fn call(
        &self,
        cx: &mut ClientContext,
        mut req: Request<B>,
    ) -> Result<Self::Response, Self::Error> {
        rewrite_uri(cx, &mut req);

        let callee = cx.rpc_info().callee();
        let address = callee.address().ok_or_else(no_address)?;

        let ver = req.version();
        let peer = PeerInfo {
            scheme: cx.target().scheme().cloned().unwrap_or(Scheme::HTTP),
            address,
            #[cfg(feature = "__tls")]
            name: callee.service_name(),
        };

        let stat_enabled = self.config.stat_enable;
        if stat_enabled {
            cx.stats.record_transport_start_at();
        }

        let mut conn = tri!(self.pooled_connect(ver, peer).await);
        let res = conn.send_request(req).await;

        if stat_enabled {
            cx.stats.record_transport_end_at();
        }

        match res {
            Ok(res) if self.idle_pool_enabled && !conn.can_share() && !conn.is_open() => {
                Ok(res.map(|body| Body::from_body(OnIdleBody::new(body, OnIdle::new(conn)))))
            }
            Ok(res) => Ok(res.map(Body::from_incoming)),
            Err(err) => Err(err),
        }
    }
}

struct OnIdle<B>
where
    B: Send + 'static,
{
    conn: Option<Pooled<PoolKey, HttpConnection<B>>>,
}

impl<B> OnIdle<B>
where
    B: Send + 'static,
{
    fn new(conn: Pooled<PoolKey, HttpConnection<B>>) -> Self {
        Self { conn: Some(conn) }
    }
}

impl<B> Future for OnIdle<B>
where
    B: Send + 'static,
{
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let result = self
            .conn
            .as_mut()
            .expect("OnIdle polled after completion")
            .poll_ready(cx);

        match result {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.conn.take();
                if result.is_err() {
                    tracing::warn!("HTTP connection closed before becoming reusable");
                }
                Poll::Ready(())
            }
        }
    }
}

#[pin_project]
struct OnIdleBody<B>
where
    B: Send + 'static,
{
    #[pin]
    body: Incoming,
    on_idle: OnIdle<B>,
    body_done: bool,
}

impl<B> OnIdleBody<B>
where
    B: Send + 'static,
{
    fn new(body: Incoming, on_idle: OnIdle<B>) -> Self {
        Self {
            body,
            on_idle,
            body_done: false,
        }
    }
}

impl<B> http_body::Body for OnIdleBody<B>
where
    B: Send + 'static,
{
    type Data = bytes::Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<hyper::Result<http_body::Frame<Self::Data>>>> {
        let mut this = self.project();

        if !*this.body_done {
            match this.body.as_mut().poll_frame(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(frame)) => return Poll::Ready(Some(frame)),
                Poll::Ready(None) => *this.body_done = true,
            }
        }

        match Pin::new(this.on_idle).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(()) => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.body_done && self.on_idle.conn.is_none()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.body.size_hint()
    }
}

enum HttpConnection<B> {
    #[cfg(feature = "http1")]
    H1(conn::http1::SendRequest<B>),
    #[cfg(feature = "http2")]
    H2(conn::http2::SendRequest<B>),
}

impl<B> Poolable for HttpConnection<B>
where
    B: Send + 'static,
{
    fn is_open(&self) -> bool {
        match &self {
            #[cfg(feature = "http1")]
            Self::H1(h1) => h1.is_ready(),
            #[cfg(feature = "http2")]
            Self::H2(h2) => h2.is_ready(),
        }
    }

    fn reserve(self) -> Reservation<Self> {
        match self {
            #[cfg(feature = "http1")]
            Self::H1(h1) => Reservation::Unique(Self::H1(h1)),
            #[cfg(feature = "http2")]
            Self::H2(h2) => Reservation::Shared(Self::H2(h2.clone()), Self::H2(h2)),
        }
    }

    fn can_share(&self) -> bool {
        match self {
            #[cfg(feature = "http1")]
            Self::H1(_) => false,
            #[cfg(feature = "http2")]
            Self::H2(_) => true,
        }
    }
}

impl<B> HttpConnection<B> {
    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<hyper::Result<()>> {
        match self {
            #[cfg(feature = "http1")]
            Self::H1(h1) => h1.poll_ready(cx),
            #[cfg(feature = "http2")]
            Self::H2(h2) => h2.poll_ready(cx),
        }
    }
}

impl<B> HttpConnection<B>
where
    B: http_body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>> + 'static,
{
    pub async fn send_request(&mut self, req: Request<B>) -> Result<Response<Incoming>> {
        let res = match self {
            #[cfg(feature = "http1")]
            Self::H1(h1) => h1.send_request(req).await,
            #[cfg(feature = "http2")]
            Self::H2(h2) => h2.send_request(req).await,
        };
        match res {
            Ok(resp) => Ok(resp),
            Err(err) => Err(request_error(err)),
        }
    }
}

static PLACEHOLDER: LazyLock<Authority> =
    LazyLock::new(|| Authority::from_static("volo-http.placeholder"));

fn gen_authority<B>(req: &Request<B>) -> Authority {
    let Some(host) = req.headers().get(header::HOST) else {
        return PLACEHOLDER.to_owned();
    };
    let Ok(host) = host.to_str() else {
        return PLACEHOLDER.to_owned();
    };
    let Ok(authority) = Authority::from_str(host) else {
        return PLACEHOLDER.to_owned();
    };
    authority
}

// We use this function for HTTP/2 only because
//
// 1. header of http2 request has a field `:scheme`, hyper demands that uri of h2 request MUST have
//    FULL uri, althrough scheme in `Uri` is optional, but authority is required.
//
//    If authority exists, hyper will set `:scheme` to HTTP if there is no scheme in `Uri`. But if
//    there is no authority, hyper will throw an error `MissingUriSchemeAndAuthority`.
//
// 2. For http2 request, hyper will ignore `Host` in `HeaderMap` and take authority as its `Host` in
//    HEADERS frame. So we must take our `Host` and set it as authority of `Uri`.
fn rewrite_uri<B>(cx: &ClientContext, req: &mut Request<B>) {
    if req.version() != Version::HTTP_2 {
        return;
    }
    let scheme = cx.target().scheme().cloned().unwrap_or(Scheme::HTTP);
    let authority = gen_authority(req);
    let mut parts = req.uri().to_owned().into_parts();
    parts.scheme = Some(scheme);
    parts.authority = Some(authority);
    let Ok(uri) = Uri::from_parts(parts) else {
        return;
    };
    *req.uri_mut() = uri;
}

#[cfg(all(test, feature = "http1"))]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use http_body_util::BodyExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    async fn read_request_head(io: &mut tokio::io::DuplexStream) {
        let mut head = Vec::new();
        let mut byte = [0; 1];
        while !head.ends_with(b"\r\n\r\n") {
            io.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
        }
        assert!(head.starts_with(b"GET / HTTP/1.1\r\n"));
    }

    fn request() -> Request {
        Request::builder()
            .uri("/")
            .header(header::HOST, "example.com")
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn on_idle_returns_http1_connection_after_response_body() {
        let (client_io, mut server_io) = tokio::io::duplex(1024);
        let (mut sender, connection) = conn::http1::handshake::<_, Body>(TokioIo::new(client_io))
            .await
            .unwrap();
        tokio::spawn(connection);
        sender.ready().await.unwrap();

        let (finish_body_tx, finish_body_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            read_request_head(&mut server_io).await;
            server_io
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhe")
                .await
                .unwrap();

            finish_body_rx.await.unwrap();
            server_io.write_all(b"llo").await.unwrap();

            read_request_head(&mut server_io).await;
            server_io
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });

        let pool = Pool::new(pool::Config::default());
        pool.no_timer();
        let key = (
            Scheme::HTTP,
            Address::Ip(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 80)),
        );
        let connecting = pool.connecting(&key, pool::Ver::Auto).unwrap();
        let mut conn = pool.pooled(connecting, HttpConnection::H1(sender));

        let response = conn.send_request(request()).await.unwrap();
        assert!(!conn.is_open());
        let response =
            response.map(|body| Body::from_body(OnIdleBody::new(body, OnIdle::new(conn))));

        let mut early_checkout = Box::pin(pool.checkout(key.clone()));
        assert!(matches!(futures::poll!(&mut early_checkout), Poll::Pending));
        drop(early_checkout);

        finish_body_tx.send(()).unwrap();
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "hello"
        );

        let mut conn = pool.checkout(key).await.unwrap();
        conn.send_request(request()).await.unwrap();
        server.await.unwrap();
    }
}
