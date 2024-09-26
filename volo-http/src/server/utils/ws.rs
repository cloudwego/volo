//! WebSocket implementation for server.
//!
//! This module provides utilities for setting up and handling WebSocket connections, including
//! configuring WebSocket options, setting protocols and upgrading connections.
//!
//! # Example
//!
//! ```
//! use std::convert::Infallible;
//!
//! use futures_util::{sink::SinkExt, stream::StreamExt};
//! use volo_http::{
//!     response::ServerResponse,
//!     server::{
//!         route::{get, Router},
//!         utils::ws::{Message, WebSocket, WebSocketUpgrade},
//!     },
//! };
//!
//! async fn handle_socket(mut socket: WebSocket) {
//!     while let Some(Ok(msg)) = socket.next().await {
//!         match msg {
//!             Message::Text(_) => {
//!                 socket.send(msg).await.unwrap();
//!             }
//!             _ => {}
//!         }
//!     }
//! }
//!
//! async fn ws_handler(ws: WebSocketUpgrade) -> ServerResponse {
//!     ws.on_upgrade(handle_socket)
//! }
//!
//! let app: Router = Router::new().route("/ws", get(ws_handler));
//! ```
//!
//! See [`WebSocketUpgrade`] and [`WebSocket`] for more details.

use std::{
    borrow::Cow,
    fmt,
    future::Future,
    ops::{Deref, DerefMut},
};

use ahash::AHashSet;
use http::{
    header,
    header::{HeaderMap, HeaderName, HeaderValue},
    method::Method,
    request::Parts,
    status::StatusCode,
    version::Version,
};
use hyper_util::rt::TokioIo;
use tokio_tungstenite::WebSocketStream;
pub use tungstenite::Message;
use tungstenite::{
    handshake::derive_accept_key,
    protocol::{self, WebSocketConfig},
};

use crate::{
    body::Body,
    context::ServerContext,
    response::ServerResponse,
    server::{extract::FromContext, IntoResponse},
};

const HEADERVALUE_UPGRADE: HeaderValue = HeaderValue::from_static("upgrade");
const HEADERVALUE_WEBSOCKET: HeaderValue = HeaderValue::from_static("websocket");

/// Handler request for establishing WebSocket connection.
///
/// [`WebSocketUpgrade`] can be passed as an argument to a handler, which will be called if the
/// http connection making the request can be upgraded to a websocket connection.
///
/// [`WebSocketUpgrade`] must be used with [`WebSocketUpgrade::on_upgrade`] and a websocket
/// handler, [`WebSocketUpgrade::on_upgrade`] will return a [`ServerResponse`] for the client and
/// the connection will then be upgraded later.
///
/// # Example
///
/// ```
/// use volo_http::{response::ServerResponse, server::utils::ws::WebSocketUpgrade};
///
/// fn ws_handler(ws: WebSocketUpgrade) -> ServerResponse {
///     ws.on_upgrade(|socket| async { todo!() })
/// }
/// ```
#[must_use]
pub struct WebSocketUpgrade<F = DefaultOnFailedUpgrade> {
    config: WebSocketConfig,
    protocol: Option<HeaderValue>,
    sec_websocket_key: HeaderValue,
    sec_websocket_protocol: Option<HeaderValue>,
    on_upgrade: hyper::upgrade::OnUpgrade,
    on_failed_upgrade: F,
}

impl<F> WebSocketUpgrade<F> {
    /// The target minimum size of the write buffer to reach before writing the data to the
    /// underlying stream.
    ///
    /// The default value is 128 KiB.
    ///
    /// If set to `0` each message will be eagerly written to the underlying stream. It is often
    /// more optimal to allow them to buffer a little, hence the default value.
    ///
    /// Note: [`flush`] will always fully write the buffer regardless.
    ///
    /// [`flush`]: futures_util::sink::SinkExt::flush
    pub fn write_buffer_size(mut self, size: usize) -> Self {
        self.config.write_buffer_size = size;
        self
    }

    /// The max size of the write buffer in bytes. Setting this can provide backpressure
    /// in the case the write buffer is filling up due to write errors.
    ///
    /// The default value is unlimited.
    ///
    /// Note: The write buffer only builds up past [`write_buffer_size`](Self::write_buffer_size)
    /// when writes to the underlying stream are failing. So the **write buffer can not
    /// fill up if you are not observing write errors even if not flushing**.
    ///
    /// Note: Should always be at least [`write_buffer_size + 1 message`](Self::write_buffer_size)
    /// and probably a little more depending on error handling strategy.
    pub fn max_write_buffer_size(mut self, max: usize) -> Self {
        self.config.max_write_buffer_size = max;
        self
    }

    /// The maximum size of an incoming message.
    ///
    /// `None` means no size limit.
    ///
    /// The default value is 64 MiB, which should be reasonably big for all normal use-cases but
    /// small enough to prevent memory eating by a malicious user.
    pub fn max_message_size(mut self, max: Option<usize>) -> Self {
        self.config.max_message_size = max;
        self
    }

    /// The maximum size of a single incoming message frame.
    ///
    /// `None` means no size limit.
    ///
    /// The limit is for frame payload NOT including the frame header.
    ///
    /// The default value is 16 MiB, which should be reasonably big for all normal use-cases but
    /// small enough to prevent memory eating by a malicious user.
    pub fn max_frame_size(mut self, max: Option<usize>) -> Self {
        self.config.max_frame_size = max;
        self
    }

    /// If server to accept unmasked frames.
    ///
    /// When set to `true`, the server will accept and handle unmasked frames from the client.
    ///
    /// According to the RFC 6455, the server must close the connection to the client in such
    /// cases, however it seems like there are some popular libraries that are sending unmasked
    /// frames, ignoring the RFC.
    ///
    /// By default this option is set to `false`, i.e. according to RFC 6455.
    pub fn accept_unmasked_frames(mut self, accept: bool) -> Self {
        self.config.accept_unmasked_frames = accept;
        self
    }

    fn get_protocol<I>(&mut self, protocols: I) -> Option<HeaderValue>
    where
        I: IntoIterator,
        I::Item: Into<Cow<'static, str>>,
    {
        let req_protocols = self
            .sec_websocket_protocol
            .as_ref()?
            .to_str()
            .ok()?
            .split(',')
            .map(str::trim)
            .collect::<AHashSet<_>>();
        for protocol in protocols.into_iter().map(Into::into) {
            if req_protocols.contains(protocol.as_ref()) {
                let protocol = match protocol {
                    Cow::Owned(s) => HeaderValue::from_str(&s).ok()?,
                    Cow::Borrowed(s) => HeaderValue::from_static(s),
                };
                return Some(protocol);
            }
        }

        None
    }

    /// Set available protocols for [`Sec-WebSocket-Protocol`][mdn].
    ///
    /// If the protocol in [`Sec-WebSocket-Protocol`][mdn] matches any protocol, the upgrade
    /// response will insert [`Sec-WebSocket-Protocol`][mdn] and [`WebSocket`] will contain the
    /// protocol name.
    ///
    /// Note that if the client offers multiple protocols that the server supports, the server will
    /// pick the first one in the list.
    ///
    /// [mdn]: https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Sec-WebSocket-Protocol
    pub fn protocols<I>(mut self, protocols: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<Cow<'static, str>>,
    {
        self.protocol = self.get_protocol(protocols);
        self
    }

    /// Provide a callback to call if upgrading the connection fails.
    ///
    /// The connection upgrade is performed in a background task. If that fails this callback will
    /// be called.
    ///
    /// By default, any errors will be silently ignored.
    ///
    /// # Example
    ///
    /// ```
    /// use volo_http::{
    ///     response::ServerResponse,
    ///     server::{
    ///         route::{get, Router},
    ///         utils::ws::{WebSocket, WebSocketUpgrade},
    ///     },
    /// };
    ///
    /// async fn ws_handler(ws: WebSocketUpgrade) -> ServerResponse {
    ///     ws.on_failed_upgrade(|err| eprintln!("Failed to upgrade connection, err: {err}"))
    ///         .on_upgrade(|socket| async { todo!() })
    /// }
    ///
    /// let router: Router = Router::new().route("/ws", get(ws_handler));
    /// ```
    pub fn on_failed_upgrade<F2>(self, callback: F2) -> WebSocketUpgrade<F2>
    where
        F2: OnFailedUpgrade,
    {
        WebSocketUpgrade {
            config: self.config,
            protocol: self.protocol,
            sec_websocket_key: self.sec_websocket_key,
            sec_websocket_protocol: self.sec_websocket_protocol,
            on_upgrade: self.on_upgrade,
            on_failed_upgrade: callback,
        }
    }

    /// Finalize upgrading the connection and call the provided callback
    ///
    /// If request protocol is matched, it will use `callback` to handle the connection stream
    /// data.
    ///
    /// The callback function should be an async function with [`WebSocket`] as parameter.
    ///
    /// # Example
    ///
    /// ```
    /// use futures_util::{sink::SinkExt, stream::StreamExt};
    /// use volo_http::{
    ///     response::ServerResponse,
    ///     server::{
    ///         route::{get, Router},
    ///         utils::ws::{WebSocket, WebSocketUpgrade},
    ///     },
    /// };
    ///
    /// async fn ws_handler(ws: WebSocketUpgrade) -> ServerResponse {
    ///     ws.on_upgrade(|mut socket| async move {
    ///         while let Some(Ok(msg)) = socket.next().await {
    ///             if msg.is_ping() || msg.is_pong() {
    ///                 continue;
    ///             }
    ///             if socket.send(msg).await.is_err() {
    ///                 break;
    ///             }
    ///         }
    ///     })
    /// }
    ///
    /// let router: Router = Router::new().route("/ws", get(ws_handler));
    /// ```
    pub fn on_upgrade<C, Fut>(self, callback: C) -> ServerResponse
    where
        C: FnOnce(WebSocket) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
        F: OnFailedUpgrade + Send + 'static,
    {
        let protocol = self.protocol.clone();
        let fut = async move {
            let upgraded = match self.on_upgrade.await {
                Ok(upgraded) => upgraded,
                Err(err) => {
                    self.on_failed_upgrade.call(WebSocketError::Upgrade(err));
                    return;
                }
            };
            let upgraded = TokioIo::new(upgraded);

            let socket = WebSocketStream::from_raw_socket(
                upgraded,
                protocol::Role::Server,
                Some(self.config),
            )
            .await;
            let socket = WebSocket {
                inner: socket,
                protocol,
            };

            callback(socket).await;
        };

        let mut resp = ServerResponse::new(Body::empty());
        *resp.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
        resp.headers_mut()
            .insert(header::CONNECTION, HEADERVALUE_UPGRADE);
        resp.headers_mut()
            .insert(header::UPGRADE, HEADERVALUE_WEBSOCKET);
        let Ok(accept_key) =
            HeaderValue::from_str(&derive_accept_key(self.sec_websocket_key.as_bytes()))
        else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        resp.headers_mut()
            .insert(header::SEC_WEBSOCKET_ACCEPT, accept_key);
        if let Some(protocol) = self.protocol {
            if let Ok(protocol) = HeaderValue::from_bytes(protocol.as_bytes()) {
                resp.headers_mut()
                    .insert(header::SEC_WEBSOCKET_PROTOCOL, protocol);
            }
        }

        tokio::spawn(fut);

        resp
    }
}

fn header_contains(headers: &HeaderMap, key: HeaderName, value: &'static str) -> bool {
    let Some(header) = headers.get(&key) else {
        return false;
    };
    let Ok(header) = simdutf8::basic::from_utf8(header.as_bytes()) else {
        return false;
    };
    header.to_ascii_lowercase().contains(value)
}

fn header_eq(headers: &HeaderMap, key: HeaderName, value: &'static str) -> bool {
    let Some(header) = headers.get(&key) else {
        return false;
    };
    header.as_bytes().eq_ignore_ascii_case(value.as_bytes())
}

impl FromContext for WebSocketUpgrade<DefaultOnFailedUpgrade> {
    type Rejection = WebSocketUpgradeRejectionError;

    async fn from_context(
        _: &mut ServerContext,
        parts: &mut Parts,
    ) -> Result<Self, Self::Rejection> {
        if parts.method != Method::GET {
            return Err(WebSocketUpgradeRejectionError::MethodNotGet);
        }
        if parts.version < Version::HTTP_11 {
            return Err(WebSocketUpgradeRejectionError::InvalidHttpVersion);
        }

        // The `Connection` may be multiple values separated by comma, so we should use
        // `header_contains` rather than `header_eq` here.
        //
        // ref: https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Connection
        if !header_contains(&parts.headers, header::CONNECTION, "upgrade") {
            return Err(WebSocketUpgradeRejectionError::InvalidConnectionHeader);
        }

        if !header_eq(&parts.headers, header::UPGRADE, "websocket") {
            return Err(WebSocketUpgradeRejectionError::InvalidUpgradeHeader);
        }

        if !header_eq(&parts.headers, header::SEC_WEBSOCKET_VERSION, "13") {
            return Err(WebSocketUpgradeRejectionError::InvalidWebSocketVersionHeader);
        }

        let sec_websocket_key = parts
            .headers
            .get(header::SEC_WEBSOCKET_KEY)
            .ok_or(WebSocketUpgradeRejectionError::WebSocketKeyHeaderMissing)?
            .clone();

        let sec_websocket_protocol = parts.headers.get(header::SEC_WEBSOCKET_PROTOCOL).cloned();

        let on_upgrade = parts
            .extensions
            .remove::<hyper::upgrade::OnUpgrade>()
            .expect("`OnUpgrade` is unavailable, maybe something wrong with `hyper`");

        Ok(Self {
            config: Default::default(),
            protocol: None,
            sec_websocket_key,
            sec_websocket_protocol,
            on_upgrade,
            on_failed_upgrade: DefaultOnFailedUpgrade,
        })
    }
}

/// WebSocketStream used In handler Request
pub struct WebSocket {
    inner: WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>,
    protocol: Option<HeaderValue>,
}

impl WebSocket {
    /// Get protocol of current websocket.
    ///
    /// The value of protocol is from [`Sec-WebSocket-Protocol`][mdn] and
    /// [`WebSocketUpgrade::protocols`] will pick one if there is any protocol that the server
    /// gived.
    ///
    /// [mdn]: https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Sec-WebSocket-Protocol
    pub fn protocol(&self) -> Option<&str> {
        simdutf8::basic::from_utf8(self.protocol.as_ref()?.as_bytes()).ok()
    }
}

impl Deref for WebSocket {
    type Target = WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for WebSocket {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Error type when using [`WebSocket`].
#[derive(Debug)]
pub enum WebSocketError {
    /// Error from [`hyper`] when calling [`OnUpgrade.await`][OnUpgrade] for upgrade a HTTP
    /// connection to a WebSocket connection.
    ///
    /// [OnUpgrade]: hyper::upgrade::OnUpgrade
    Upgrade(hyper::Error),
}

impl fmt::Display for WebSocketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Upgrade(err) => write!(f, "failed to upgrade: {err}"),
        }
    }
}

impl std::error::Error for WebSocketError {}

/// What to do when a connection upgrade fails.
///
/// See [`WebSocketUpgrade::on_failed_upgrade`] for more details.
pub trait OnFailedUpgrade {
    /// Called when a connection upgrade fails.
    fn call(self, error: WebSocketError);
}

impl<F> OnFailedUpgrade for F
where
    F: FnOnce(WebSocketError),
{
    fn call(self, error: WebSocketError) {
        self(error)
    }
}

/// The default `OnFailedUpgrade` used by `WebSocketUpgrade`.
///
/// It simply ignores the error.
#[derive(Debug)]
pub struct DefaultOnFailedUpgrade;

impl OnFailedUpgrade for DefaultOnFailedUpgrade {
    fn call(self, _: WebSocketError) {}
}

/// [`Error`]s while extracting [`WebSocketUpgrade`].
///
/// [`Error`]: std::error::Error
/// [`WebSocketUpgrade`]: crate::server::utils::ws::WebSocketUpgrade
#[derive(Debug)]
pub enum WebSocketUpgradeRejectionError {
    /// The request method must be `GET`
    MethodNotGet,
    /// The HTTP version is not supported
    InvalidHttpVersion,
    /// The `Connection` header is invalid
    InvalidConnectionHeader,
    /// The `Upgrade` header is invalid
    InvalidUpgradeHeader,
    /// The `Sec-WebSocket-Version` header is invalid
    InvalidWebSocketVersionHeader,
    /// The `Sec-WebSocket-Key` header is missing
    WebSocketKeyHeaderMissing,
}

impl WebSocketUpgradeRejectionError {
    /// Convert the [`WebSocketUpgradeRejectionError`] to the corresponding [`StatusCode`]
    fn to_status_code(&self) -> StatusCode {
        match self {
            Self::MethodNotGet => StatusCode::METHOD_NOT_ALLOWED,
            Self::InvalidHttpVersion => StatusCode::HTTP_VERSION_NOT_SUPPORTED,
            Self::InvalidConnectionHeader => StatusCode::UPGRADE_REQUIRED,
            Self::InvalidUpgradeHeader => StatusCode::BAD_REQUEST,
            Self::InvalidWebSocketVersionHeader => StatusCode::BAD_REQUEST,
            Self::WebSocketKeyHeaderMissing => StatusCode::BAD_REQUEST,
        }
    }
}

impl std::error::Error for WebSocketUpgradeRejectionError {}

impl fmt::Display for WebSocketUpgradeRejectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MethodNotGet => f.write_str("Request method must be `GET`"),
            Self::InvalidHttpVersion => f.write_str("HTTP version not support"),
            Self::InvalidConnectionHeader => {
                f.write_str("Header `Connection` does not include `upgrade`")
            }
            Self::InvalidUpgradeHeader => f.write_str("Header `Upgrade` is not `websocket`"),
            Self::InvalidWebSocketVersionHeader => {
                f.write_str("Header `Sec-WebSocket-Version` is not `13`")
            }
            Self::WebSocketKeyHeaderMissing => f.write_str("Header `Sec-WebSocket-Key` is missing"),
        }
    }
}

impl IntoResponse for WebSocketUpgradeRejectionError {
    fn into_response(self) -> ServerResponse {
        self.to_status_code().into_response()
    }
}

#[cfg(test)]
mod websocket_tests {
    use std::{
        convert::Infallible,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        str::FromStr,
    };

    use futures_util::{sink::SinkExt, stream::StreamExt};
    use http::uri::Uri;
    use motore::service::Service;
    use tokio::net::TcpStream;
    use tokio_tungstenite::MaybeTlsStream;
    use tungstenite::ClientRequestBuilder;
    use volo::net::Address;

    use super::*;
    use crate::{request::ServerRequest, server::test_helpers, Server};

    fn simple_parts() -> Parts {
        let req = ServerRequest::builder()
            .method(Method::GET)
            .version(Version::HTTP_11)
            .header(header::HOST, "localhost")
            .header(header::CONNECTION, super::HEADERVALUE_UPGRADE)
            .header(header::UPGRADE, super::HEADERVALUE_WEBSOCKET)
            .header(header::SEC_WEBSOCKET_KEY, "6D69KGBOr4Re+Nj6zx9aQA==")
            .header(header::SEC_WEBSOCKET_VERSION, "13")
            .body(())
            .unwrap();
        req.into_parts().0
    }

    async fn run_ws_handler<S>(
        service: S,
        sub_protocol: Option<&'static str>,
        port: u16,
    ) -> (
        WebSocketStream<MaybeTlsStream<TcpStream>>,
        ServerResponse<Option<Vec<u8>>>,
    )
    where
        S: Service<ServerContext, ServerRequest, Response = ServerResponse, Error = Infallible>
            + Send
            + Sync
            + 'static,
    {
        let addr = Address::Ip(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            port,
        ));
        tokio::spawn(Server::new(service).run(addr.clone()));

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let mut req = ClientRequestBuilder::new(Uri::from_str(&format!("ws://{addr}/")).unwrap());
        if let Some(sub_protocol) = sub_protocol {
            req = req.with_sub_protocol(sub_protocol);
        }
        tokio_tungstenite::connect_async(req).await.unwrap()
    }

    #[tokio::test]
    async fn rejection() {
        {
            let mut parts = simple_parts();
            parts.method = Method::POST;
            let res =
                WebSocketUpgrade::from_context(&mut test_helpers::empty_cx(), &mut parts).await;
            assert!(matches!(
                res,
                Err(WebSocketUpgradeRejectionError::MethodNotGet)
            ));
        }
        {
            let mut parts = simple_parts();
            parts.version = Version::HTTP_10;
            let res =
                WebSocketUpgrade::from_context(&mut test_helpers::empty_cx(), &mut parts).await;
            assert!(matches!(
                res,
                Err(WebSocketUpgradeRejectionError::InvalidHttpVersion)
            ));
        }
        {
            let mut parts = simple_parts();
            parts.headers.remove(header::CONNECTION);
            let res =
                WebSocketUpgrade::from_context(&mut test_helpers::empty_cx(), &mut parts).await;
            assert!(matches!(
                res,
                Err(WebSocketUpgradeRejectionError::InvalidConnectionHeader)
            ));
        }
        {
            let mut parts = simple_parts();
            parts.headers.remove(header::CONNECTION);
            parts
                .headers
                .insert(header::CONNECTION, HeaderValue::from_static("downgrade"));
            let res =
                WebSocketUpgrade::from_context(&mut test_helpers::empty_cx(), &mut parts).await;
            assert!(matches!(
                res,
                Err(WebSocketUpgradeRejectionError::InvalidConnectionHeader)
            ));
        }
        {
            let mut parts = simple_parts();
            parts.headers.remove(header::UPGRADE);
            let res =
                WebSocketUpgrade::from_context(&mut test_helpers::empty_cx(), &mut parts).await;
            assert!(matches!(
                res,
                Err(WebSocketUpgradeRejectionError::InvalidUpgradeHeader)
            ));
        }
        {
            let mut parts = simple_parts();
            parts.headers.remove(header::UPGRADE);
            parts
                .headers
                .insert(header::UPGRADE, HeaderValue::from_static("supersocket"));
            let res =
                WebSocketUpgrade::from_context(&mut test_helpers::empty_cx(), &mut parts).await;
            assert!(matches!(
                res,
                Err(WebSocketUpgradeRejectionError::InvalidUpgradeHeader)
            ));
        }
        {
            let mut parts = simple_parts();
            parts.headers.remove(header::SEC_WEBSOCKET_VERSION);
            let res =
                WebSocketUpgrade::from_context(&mut test_helpers::empty_cx(), &mut parts).await;
            assert!(matches!(
                res,
                Err(WebSocketUpgradeRejectionError::InvalidWebSocketVersionHeader)
            ));
        }
        {
            let mut parts = simple_parts();
            parts.headers.remove(header::SEC_WEBSOCKET_VERSION);
            parts.headers.insert(
                header::SEC_WEBSOCKET_VERSION,
                HeaderValue::from_static("114514"),
            );
            let res =
                WebSocketUpgrade::from_context(&mut test_helpers::empty_cx(), &mut parts).await;
            assert!(matches!(
                res,
                Err(WebSocketUpgradeRejectionError::InvalidWebSocketVersionHeader)
            ));
        }
        {
            let mut parts = simple_parts();
            parts.headers.remove(header::SEC_WEBSOCKET_KEY);
            let res =
                WebSocketUpgrade::from_context(&mut test_helpers::empty_cx(), &mut parts).await;
            assert!(matches!(
                res,
                Err(WebSocketUpgradeRejectionError::WebSocketKeyHeaderMissing)
            ));
        }
    }

    #[tokio::test]
    async fn protocol_test() {
        async fn handler(ws: WebSocketUpgrade) -> ServerResponse {
            ws.protocols(["soap", "wmap", "graphql-ws", "chat"])
                .on_upgrade(|_| async {})
        }

        let (_, resp) =
            run_ws_handler(test_helpers::to_service(handler), Some("graphql-ws"), 25230).await;

        assert_eq!(
            resp.headers()
                .get(http::header::SEC_WEBSOCKET_PROTOCOL)
                .unwrap(),
            "graphql-ws"
        );
    }

    #[tokio::test]
    async fn success_on_upgrade() {
        async fn echo(mut socket: WebSocket) {
            while let Some(Ok(msg)) = socket.next().await {
                if msg.is_ping() || msg.is_pong() {
                    continue;
                }
                if socket.send(msg).await.is_err() {
                    break;
                }
            }
        }

        async fn handler(ws: WebSocketUpgrade) -> ServerResponse {
            ws.on_upgrade(echo)
        }

        let (mut ws_stream, _) =
            run_ws_handler(test_helpers::to_service(handler), None, 25231).await;

        let input = Message::Text("foobar".to_owned());
        ws_stream.send(input.clone()).await.unwrap();
        let output = ws_stream.next().await.unwrap().unwrap();
        assert_eq!(input, output);

        let input = Message::Ping("foobar".to_owned().into_bytes());
        ws_stream.send(input).await.unwrap();
        let output = ws_stream.next().await.unwrap().unwrap();
        assert_eq!(output, Message::Pong("foobar".to_owned().into_bytes()));
    }
}
