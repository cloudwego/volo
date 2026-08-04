//! Protocol related implementations

use std::{error::Error, str::FromStr, sync::LazyLock};

use futures::{
    FutureExt, TryFutureExt,
    future::{self, Either},
};
use http::{
    header,
    uri::{Authority, Scheme, Uri},
    version::Version,
};
use hyper::client::conn;
use hyper_util::rt::TokioIo;
use motore::{make::MakeConnection, service::Service};
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

#[cfg(feature = "__tls")]
type PoolKey = (Scheme, Address, Option<faststr::FastStr>);

#[cfg(not(feature = "__tls"))]
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
        let key = pool_key(&peer);
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
        let key = pool_key(&peer);

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

fn pool_key(peer: &PeerInfo) -> PoolKey {
    (
        peer.scheme.clone(),
        peer.address.clone(),
        #[cfg(feature = "__tls")]
        (peer.scheme == Scheme::HTTPS).then(|| peer.name.clone()),
    )
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
    #[cfg(feature = "http1")]
    let key = pool_key(&peer);

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

            // This channel only returns the sender from the request future to the
            // connection task.
            let (return_tx, return_rx) = tokio::sync::mpsc::unbounded_channel();

            // Replace `tokio::spawn(connection)` with a managed wrapper.
            let driver = ManagedH1Connection {
                connection: conn,
                return_rx,
                waiting: None,
                returner: pool.return_handle(),
                key,
            };

            tokio::spawn(driver);

            // The connect future still owns return_tx, so return_rx cannot close
            // before the initial readiness check completes.
            tri!(sender.ready().await.map_err(connect_error));

            let lease = H1Lease::new(sender, return_tx);
            Ok(pool.pooled(connecting, HttpConnection::H1(lease)))
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

        if res.is_ok() && self.idle_pool_enabled && conn.should_wait_until_ready() {
            tokio::spawn(async move {
                if conn.ready().await.is_err() {
                    tracing::warn!("HTTP connection closed before becoming reusable");
                }
            });
        }

        res
    }
}

#[cfg(feature = "http1")]
struct H1Returned<B> {
    http_sender: conn::http1::SendRequest<B>,
    return_tx: tokio::sync::mpsc::UnboundedSender<H1Returned<B>>,
}

#[cfg(feature = "http1")]
struct H1Lease<B> {
    returned: Option<H1Returned<B>>,
}

#[cfg(feature = "http1")]
struct H1ReturnGuard<B> {
    returned: Option<H1Returned<B>>,
}

#[cfg(feature = "http1")]
impl<B> H1ReturnGuard<B> {
    fn http_sender_mut(&mut self) -> &mut conn::http1::SendRequest<B> {
        &mut self
            .returned
            .as_mut()
            .expect("HTTP/1 sender already returned")
            .http_sender
    }

    fn return_to_driver(&mut self) {
        let Some(returned) = self.returned.take() else {
            return;
        };

        // The original tx must keep moving with the message. Use a temporary
        // clone to perform this send.
        let tx = returned.return_tx.clone();
        if let Err(_err) = tx.send(returned) {
            // The receiver is gone, so the driver for this physical connection
            // has already stopped. There is no receiver to retry, and a sender
            // whose readiness is unknown must not be returned directly to the
            // pool. Dropping SendError also drops the sender and return_tx,
            // explicitly giving up reuse of this connection.
            tracing::trace!("HTTP/1 connection driver already closed");
        };
    }
}

#[cfg(feature = "http1")]
impl<B> Drop for H1ReturnGuard<B> {
    fn drop(&mut self) {
        // Cancellation of the send_request future uses the same return path.
        self.return_to_driver();
    }
}

#[cfg(feature = "http1")]
impl<B> H1Lease<B> {
    fn new(
        http_sender: conn::http1::SendRequest<B>,
        return_tx: tokio::sync::mpsc::UnboundedSender<H1Returned<B>>,
    ) -> Self {
        Self {
            returned: Some(H1Returned {
                http_sender,
                return_tx,
            }),
        }
    }

    fn from_returned(returned: H1Returned<B>) -> Self {
        Self {
            returned: Some(returned),
        }
    }

    fn is_ready(&self) -> bool {
        self.returned
            .as_ref()
            .is_some_and(|returned| returned.http_sender.is_ready())
    }

    async fn send_request(
        &mut self,
        req: Request<B>,
    ) -> hyper::Result<http::Response<hyper::body::Incoming>>
    where
        B: http_body::Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<BoxError> + 'static,
    {
        // Once the sender is taken, this old lease can never re-enter the pool.
        let returned = self
            .returned
            .take()
            .expect("an HTTP/1 lease can only send one request");

        let mut guard = H1ReturnGuard {
            returned: Some(returned),
        };

        let result = guard.http_sender_mut().send_request(req).await;

        // Return the sender before the response is handed to the caller.
        guard.return_to_driver();
        result
    }
}

#[cfg(feature = "http1")]
#[pin_project::pin_project]
struct ManagedH1Connection<I, B>
where
    I: hyper::rt::Read + hyper::rt::Write,
    B: http_body::Body + Send + 'static,
{
    #[pin]
    connection: conn::http1::Connection<I, B>,
    return_rx: tokio::sync::mpsc::UnboundedReceiver<H1Returned<B>>,
    waiting: Option<H1Returned<B>>,
    returner: pool::PoolReturn<PoolKey, HttpConnection<B>>,
    key: PoolKey,
}

#[cfg(feature = "http1")]
impl<I, B> Future for ManagedH1Connection<I, B>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
    B: http_body::Body + Unpin + Send + 'static,
    B::Data: Send,
    B::Error: Into<BoxError> + 'static,
{
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        use std::task::Poll;

        let this = self.project();

        // 1. Receive the sender returned by the request future.
        if this.waiting.is_none() {
            match this.return_rx.poll_recv(cx) {
                Poll::Ready(Some(returned)) => {
                    *this.waiting = Some(returned);
                }
                Poll::Ready(None) => {
                    // No lease, guard, queued message, or waiting sender remains.
                    tracing::trace!("HTTP/1 return channel closed");
                    return Poll::Ready(());
                }
                Poll::Pending => {}
            }
        }

        // 2. Drive the actual HTTP/1 socket I/O and state machine.
        match this.connection.poll(cx) {
            Poll::Ready(Ok(())) => {
                tracing::trace!("HTTP/1 connection driver completed");
                return Poll::Ready(());
            }
            Poll::Ready(Err(err)) => {
                tracing::trace!("HTTP/1 connection driver failed: {err}");
                return Poll::Ready(());
            }
            Poll::Pending => {}
        }

        // 3. If no sender has arrived, only the connection needs driving.
        let Some(returned) = this.waiting.as_mut() else {
            // Reaching this branch means poll_recv returned Pending above and
            // registered cx.waker(). Connection::poll also returned Pending,
            // so either the return channel or Hyper I/O will wake this task.
            return Poll::Pending;
        };

        // 4. Wait until Hyper explicitly reports that the sender is reusable.
        // poll_ready also registers this driver's waker when it returns Pending.
        match returned.http_sender.poll_ready(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(err)) => {
                tracing::trace!("HTTP/1 sender closed before becoming reusable: {err}");
                return Poll::Ready(());
            }
            Poll::Ready(Ok(())) => {}
        }

        // 5. Once truly ready, create the next lease and return it to the pool.
        let returned = this
            .waiting
            .take()
            .expect("HTTP/1 waiting sender disappeared");
        let http_connection = HttpConnection::H1(H1Lease::from_returned(returned));

        if let Err(_err) = this.returner.put_ready(this.key.clone(), http_connection) {
            // The response may have completed successfully, but the pool no longer
            // exists. Drop the returned sender and stop driving this connection.
            tracing::trace!("HTTP/1 pool dropped before the connection became reusable");
            return Poll::Ready(());
        }

        // 6. Re-register the receiver waker for the next return or for the last
        // tx being dropped.
        match this.return_rx.poll_recv(cx) {
            Poll::Ready(Some(returned)) => {
                debug_assert!(this.waiting.is_none());
                *this.waiting = Some(returned);
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Poll::Ready(None) => {
                // For example, max_idle_per_host=0 drops the new lease at once.
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

enum HttpConnection<B> {
    #[cfg(feature = "http1")]
    H1(H1Lease<B>),
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

impl<B> HttpConnection<B>
where
    B: http_body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>> + 'static,
{
    fn should_wait_until_ready(&self) -> bool {
        match self {
            #[cfg(feature = "http1")]
            Self::H1(h1) => !h1.is_ready(),
            #[cfg(feature = "http2")]
            Self::H2(h2) => false,
        }
    }

    async fn ready(&mut self) -> hyper::Result<()> {
        match self {
            #[cfg(feature = "http1")]
            Self::H1(h1) => h1.ready().await,
            #[cfg(feature = "http2")]
            Self::H2(h2) => h2.ready().await,
        }
    }

    pub async fn send_request(&mut self, req: Request<B>) -> Result<Response> {
        let res = match self {
            #[cfg(feature = "http1")]
            Self::H1(h1) => h1.send_request(req).await,
            #[cfg(feature = "http2")]
            Self::H2(h2) => h2.send_request(req).await,
        };
        match res {
            Ok(resp) => Ok(resp.map(Body::from_incoming)),
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
mod h1_connection_reuse_tests {
    use std::{
        future::{Future, pending, poll_fn},
        net::{Ipv4Addr, SocketAddr},
        task::Poll,
        time::Duration,
    };

    use bytes::Bytes;
    use http::{Request, header::HOST, uri::Scheme};
    use http_body_util::Empty;
    use hyper::client::conn;
    use hyper_util::rt::TokioIo;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
        sync::{mpsc, oneshot},
        time::timeout,
    };
    use volo::net::Address;

    use super::{H1Lease, HttpConnection, ManagedH1Connection, PoolKey, pool};
    use crate::body::BodyConversion;

    fn pool_key() -> PoolKey {
        (
            Scheme::HTTP,
            Address::Ip(SocketAddr::from((Ipv4Addr::LOCALHOST, 80))),
            #[cfg(feature = "__tls")]
            None,
        )
    }

    fn empty_request() -> Request<Empty<Bytes>> {
        Request::builder()
            .method("GET")
            .uri("/")
            .header(HOST, "example.test")
            .body(Empty::new())
            .expect("valid request")
    }

    async fn read_request_head(io: &mut DuplexStream) {
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];

        loop {
            let read = io.read(&mut byte).await.expect("read request");
            assert_ne!(read, 0, "client closed before sending a full request");
            head.push(byte[0]);

            if head.ends_with(b"\r\n\r\n") {
                return;
            }
        }
    }

    #[tokio::test]
    async fn managed_driver_reuses_only_after_response_body_eof() {
        let (client_io, mut server_io) = tokio::io::duplex(4096);
        let (head_sent_tx, head_sent_rx) = oneshot::channel();
        let (release_body_tx, release_body_rx) = oneshot::channel();

        let server_task = tokio::spawn(async move {
            read_request_head(&mut server_io).await;
            server_io
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n")
                .await
                .expect("write first response head");
            head_sent_tx.send(()).expect("signal response head");

            release_body_rx.await.expect("release first response body");
            server_io
                .write_all(b"pong")
                .await
                .expect("write first response body");

            // A second request on the same DuplexStream proves physical reuse.
            read_request_head(&mut server_io).await;
            server_io
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .expect("write second response");

            pending::<()>().await;
        });

        let (mut sender, connection) =
            conn::http1::handshake::<_, Empty<Bytes>>(TokioIo::new(client_io))
                .await
                .expect("HTTP/1 handshake");

        let pool = pool::Pool::new(pool::Config {
            idle_timeout: Duration::from_secs(60),
            max_idle_per_host: 16,
        });
        let key = pool_key();
        let (return_tx, return_rx) = mpsc::unbounded_channel();

        let driver_task = tokio::spawn(ManagedH1Connection {
            connection,
            return_rx,
            waiting: None,
            returner: pool.return_handle(),
            key: key.clone(),
        });

        sender.ready().await.expect("initial sender readiness");
        let connecting = pool
            .connecting(&key, pool::Ver::Auto)
            .expect("HTTP/1 does not serialize connects");
        let mut pooled = pool.pooled(
            connecting,
            HttpConnection::H1(H1Lease::new(sender, return_tx)),
        );

        let response = pooled
            .send_request(empty_request())
            .await
            .expect("first response head");
        head_sent_rx.await.expect("server sent response head");

        // Poll checkout exactly once. The body is still blocked by the server,
        // so returning a connection here would be premature reuse.
        let mut early_checkout = Box::pin(pool.checkout(key.clone()));
        let returned_too_early =
            poll_fn(|cx| Poll::Ready(matches!(early_checkout.as_mut().poll(cx), Poll::Ready(_))))
                .await;
        assert!(!returned_too_early);
        drop(early_checkout);

        release_body_tx.send(()).expect("release response body");
        assert_eq!(
            response
                .into_body()
                .into_vec()
                .await
                .expect("collect first response"),
            b"pong"
        );

        // The old Pooled contains an empty H1Lease and must not reinsert itself.
        drop(pooled);

        let mut reused = timeout(Duration::from_secs(1), pool.checkout(key.clone()))
            .await
            .expect("driver did not return the ready connection")
            .expect("checkout failed");

        let second = reused
            .send_request(empty_request())
            .await
            .expect("second response head");
        assert_eq!(
            second
                .into_body()
                .into_vec()
                .await
                .expect("collect second response"),
            b"ok"
        );

        drop(reused);
        drop(pool);

        timeout(Duration::from_secs(1), driver_task)
            .await
            .expect("driver should stop after the pool is dropped")
            .expect("driver task panicked");
        server_task.abort();
    }

    #[tokio::test]
    async fn canceled_send_returns_sender_and_consumes_old_lease() {
        let (client_io, mut server_io) = tokio::io::duplex(4096);
        let (request_seen_tx, request_seen_rx) = oneshot::channel();

        let server_task = tokio::spawn(async move {
            read_request_head(&mut server_io).await;
            request_seen_tx.send(()).expect("signal request received");

            // Keep the request future waiting for a response head.
            pending::<()>().await;
        });

        let (mut sender, connection) =
            conn::http1::handshake::<_, Empty<Bytes>>(TokioIo::new(client_io))
                .await
                .expect("HTTP/1 handshake");
        let connection_task = tokio::spawn(async move {
            let _ = connection.await;
        });

        sender.ready().await.expect("initial sender readiness");
        let (return_tx, mut return_rx) = mpsc::unbounded_channel();
        let mut lease = H1Lease::new(sender, return_tx);
        let mut send = Box::pin(lease.send_request(empty_request()));

        tokio::select! {
            result = &mut send => panic!("request completed unexpectedly: {result:?}"),
            result = request_seen_rx => result.expect("server observed the request"),
        }

        // Dropping the future must run H1ReturnGuard::drop.
        drop(send);

        assert!(
            lease.returned.is_none(),
            "the old lease must permanently lose its sender"
        );

        let returned = timeout(Duration::from_secs(1), return_rx.recv())
            .await
            .expect("guard did not return the sender")
            .expect("return channel closed unexpectedly");

        assert!(matches!(
            return_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        drop(returned);
        connection_task.abort();
        server_task.abort();
    }
}
