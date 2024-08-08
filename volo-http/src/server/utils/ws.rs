//! Handle WebSocket connection
//!
//! This module provides utilities for setting up and handling WebSocket connections, including
//! configuring WebSocket options, setting protocols, and upgrading connections.
//!
//! It uses [`hyper::upgrade::OnUpgrade`] to upgrade the connection.
//!
//! # Example
//!
//! ```rust
//! use std::convert::Infallible;
//!
//! use futures_util::{SinkExt, StreamExt};
//! use volo_http::{
//!     response::ServerResponse,
//!     server::{
//!         route::get,
//!         utils::{Message, WebSocket, WebSocketUpgrade},
//!     },
//!     Router,
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
//! let app: Router<ServerResponse, Infallible> = Router::new().route("/ws", get(ws_handler));
//! ```

use std::{borrow::Cow, fmt::Formatter, future::Future};

use http::{request::Parts, HeaderMap, HeaderName, HeaderValue};
use hyper::Error;
use hyper_util::rt::TokioIo;
use tokio_tungstenite::{
    tungstenite::{
        self,
        handshake::derive_accept_key,
        protocol::{self, WebSocketConfig},
    },
    WebSocketStream,
};

use crate::{
    body::Body, context::ServerContext, error::server::WebSocketUpgradeRejectionError,
    response::ServerResponse, server::extract::FromContext,
};

/// WebSocketStream used In handler Request
pub type WebSocket = WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>;
/// alias of [`tungstenite::Message`]
pub type Message = tungstenite::Message;

/// WebSocket Request headers for establishing a WebSocket connection.
struct Headers {
    /// The `Sec-WebSocket-Key` request header value
    /// used for compute 'Sec-WebSocket-Accept' response header value
    sec_websocket_key: HeaderValue,
    /// The `Sec-WebSocket-Protocol` request header value
    /// specify [`Callback`] method depend on the protocol
    sec_websocket_protocol: Option<HeaderValue>,
}

impl std::fmt::Debug for Headers {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Headers")
            .field("sec_websocket_key", &self.sec_websocket_protocol)
            .field("sec_websocket_protocol", &self.sec_websocket_protocol)
            .finish_non_exhaustive()
    }
}

/// WebSocket config
#[derive(Default)]
pub struct Config {
    /// WebSocket config for transport (alias of
    /// [`WebSocketConfig`](tungstenite::protocol::WebSocketConfig)) e.g. max write buffer size
    transport: WebSocketConfig,
    /// The chosen protocol sent in the `Sec-WebSocket-Protocol` header of the response.
    /// use [`WebSocketUpgrade::protocols`] to set server supported protocols
    protocols: Vec<HeaderValue>,
}

impl Config {
    /// Create Default Config
    pub fn new() -> Self {
        Config {
            transport: WebSocketConfig::default(),
            protocols: Vec::new(),
        }
    }

    /// Set server supported protocols.
    ///
    /// This will filter protocols in request header `Sec-WebSocket-Protocol`
    /// and will set the first server supported protocol in [`http::header::Sec-WebSocket-Protocol`]
    /// in response
    ///
    /// ```rust
    /// use volo_http::server::utils::WebSocketConfig;
    ///
    /// let config = WebSocketConfig::new().set_protocols(["graphql-ws", "graphql-transport-ws"]);
    /// ```
    pub fn set_protocols<I>(mut self, protocols: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<Cow<'static, str>>,
    {
        self.protocols = protocols
            .into_iter()
            .map(Into::into)
            .map(|protocol| match protocol {
                Cow::Owned(s) => HeaderValue::from_str(&s).unwrap(),
                Cow::Borrowed(s) => HeaderValue::from_static(s),
            })
            .collect();
        self
    }

    /// Set transport config
    ///
    /// e.g. write buffer size
    ///
    /// ```rust
    /// use tokio_tungstenite::tungstenite::protocol::WebSocketConfig as WebSocketTransConfig;
    /// use volo_http::server::utils::WebSocketConfig;
    ///
    /// let config = WebSocketConfig::new().set_transport(WebSocketTransConfig {
    ///     write_buffer_size: 128 * 1024,
    ///     ..<_>::default()
    /// });
    /// ```
    pub fn set_transport(mut self, config: WebSocketConfig) -> Self {
        self.transport = config;
        self
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("transport", &self.transport)
            .field("protocols", &self.protocols)
            .finish_non_exhaustive()
    }
}

/// Callback fn that processes [`WebSocket`]
pub trait Callback: Send + 'static {
    /// Called when a connection upgrade succeeds
    fn call(self, _: WebSocket) -> impl Future<Output = ()> + Send;
}

impl<Fut, C> Callback for C
where
    Fut: Future<Output = ()> + Send + 'static,
    C: FnOnce(WebSocket) -> Fut + Send + Copy + 'static,
{
    async fn call(self, websocket: WebSocket) {
        self(websocket).await;
    }
}

/// What to do when a connection upgrade fails.
///
/// See [`WebSocketUpgrade::on_failed_upgrade`] for more details.
pub trait OnFailedUpgrade: Send + 'static {
    /// Called when a connection upgrade fails.
    fn call(self, error: Error);
}

impl<F> OnFailedUpgrade for F
where
    F: FnOnce(Error) + Send + 'static,
{
    fn call(self, error: Error) {
        self(error)
    }
}

/// The default `OnFailedUpgrade` used by `WebSocketUpgrade`.
///
/// It simply ignores the error.
#[non_exhaustive]
#[derive(Debug)]
pub struct DefaultOnFailedUpgrade;

impl OnFailedUpgrade for DefaultOnFailedUpgrade {
    #[inline]
    fn call(self, _error: Error) {}
}

/// The default `Callback` used by `WebSocketUpgrade`.
///
/// It simply ignores the socket.
#[derive(Clone)]
pub struct DefaultCallback;
impl Callback for DefaultCallback {
    #[inline]
    async fn call(self, _: WebSocket) {}
}

/// Extractor of [`FromContext`] for establishing WebSocket connection
///
/// **Constrains**:
///
/// The extractor only supports for the request that has the method [`GET`](http::method::GET)
/// and contains certain header values.
///
/// See more details in [`WebSocketUpgrade::from_context`]
///
/// # Usage
///
/// ```rust
/// use volo_http::{response::ServerResponse, server::utils::WebSocketUpgrade};
///
/// fn ws_handler(ws: WebSocketUpgrade) -> ServerResponse {
///     ws.on_upgrade(|socket| async { unimplemented!() })
/// }
/// ```
pub struct WebSocketUpgrade<F = DefaultOnFailedUpgrade> {
    config: Config,
    on_failed_upgrade: F,
    on_upgrade: hyper::upgrade::OnUpgrade,
    headers: Headers,
}

impl<F> std::fmt::Debug for WebSocketUpgrade<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSocketUpgrade")
            .field("config", &self.config)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

impl<F> WebSocketUpgrade<F>
where
    F: OnFailedUpgrade,
{
    /// Set WebSocket config
    /// ```rust
    /// use volo_http::{
    ///     response::ServerResponse,
    ///     server::utils::{
    ///         WebSocketConfig,
    ///         WebSocketUpgrade,
    ///     }
    /// };
    /// use tokio_tungstenite::tungstenite::protocol::{WebSocketConfig as WebSocketTransConfig};
    ///
    /// async fn ws_handler(ws: WebSocketUpgrade) -> ServerResponse{
    ///     ws.set_config(
    ///         WebSocketConfig::new()
    ///             .set_protocols(["graphql-ws","graphql-transport-ws"])
    ///             .set_transport(
    ///                 WebSocketTransConfig{
    ///                     write_buffer_size: 128 * 1024,
    ///                     ..<_>::default()
    ///                 }
    ///             )
    ///         )
    ///         .on_upgrade(|socket| async{} )
    /// }
    pub fn set_config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    /// Provide a callback to call if upgrading the connection fails.
    ///
    /// The connection upgrade is performed in a background task.
    /// If that fails this callback will be called.
    ///
    /// By default, any errors will be silently ignored.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::collections::HashMap;
    /// use volo_http::{
    ///     response::ServerResponse,
    ///     server::utils::{
    ///         WebSocketConfig,
    ///         WebSocketUpgrade,
    ///         WebSocket,
    ///     }
    /// };
    ///
    /// async fn ws_handler(ws: WebSocketUpgrade) -> ServerResponse{
    ///     ws.on_failed_upgrade(|error| {
    ///             unimplemented!()
    ///         })
    ///         .on_upgrade(|socket| async{} )
    /// }
    pub fn on_failed_upgrade<F1>(self, callback: F1) -> WebSocketUpgrade<F1>
    where
        F1: OnFailedUpgrade,
    {
        WebSocketUpgrade {
            config: self.config,
            on_failed_upgrade: callback,
            on_upgrade: self.on_upgrade,
            headers: self.headers,
        }
    }

    /// Finalize upgrading the connection and call the provided callback
    /// if request protocol is matched, it will use `callback` to handle the connection stream data
    pub fn on_upgrade<Fut, C>(self, callback: C) -> ServerResponse
    where
        Fut: Future<Output = ()> + Send + 'static,
        C: FnOnce(WebSocket) -> Fut + Send + Sync + 'static,
    {
        let on_upgrade = self.on_upgrade;
        let config = self.config.transport;
        let on_failed_upgrade = self.on_failed_upgrade;

        let protocol = self
            .headers
            .sec_websocket_protocol
            .clone()
            .as_ref()
            .and_then(|p| p.to_str().ok())
            .and_then(|req_protocols| {
                self.config.protocols.iter().find(|protocol| {
                    req_protocols
                        .split(',')
                        .any(|req_protocol| req_protocol == *protocol)
                })
            });

        tokio::spawn(async move {
            let upgraded = match on_upgrade.await {
                Ok(upgraded) => upgraded,
                Err(err) => {
                    on_failed_upgrade.call(err);
                    return;
                }
            };
            let upgraded = TokioIo::new(upgraded);

            let socket =
                WebSocketStream::from_raw_socket(upgraded, protocol::Role::Server, Some(config))
                    .await;

            callback(socket).await;
        });

        const UPGRADE: HeaderValue = HeaderValue::from_static("upgrade");
        const WEBSOCKET: HeaderValue = HeaderValue::from_static("websocket");

        let mut builder = ServerResponse::builder()
            .status(http::StatusCode::SWITCHING_PROTOCOLS)
            .header(http::header::CONNECTION, UPGRADE)
            .header(http::header::UPGRADE, WEBSOCKET)
            .header(
                http::header::SEC_WEBSOCKET_ACCEPT,
                derive_accept_key(self.headers.sec_websocket_key.as_bytes()),
            );

        if let Some(protocol) = protocol {
            builder = builder.header(http::header::SEC_WEBSOCKET_PROTOCOL, protocol);
        }

        builder.body(Body::empty()).unwrap()
    }
}

fn header_contains(headers: &HeaderMap, key: HeaderName, value: &'static str) -> bool {
    let header = if let Some(header) = headers.get(&key) {
        header
    } else {
        return false;
    };

    if let Ok(header) = std::str::from_utf8(header.as_bytes()) {
        header.to_ascii_lowercase().contains(value)
    } else {
        false
    }
}

fn header_eq(headers: &HeaderMap, key: HeaderName, value: &'static str) -> bool {
    if let Some(header) = headers.get(&key) {
        header.as_bytes().eq_ignore_ascii_case(value.as_bytes())
    } else {
        false
    }
}

impl FromContext for WebSocketUpgrade<DefaultOnFailedUpgrade> {
    type Rejection = WebSocketUpgradeRejectionError;

    async fn from_context(
        _cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> Result<Self, Self::Rejection> {
        if parts.method != http::Method::GET {
            return Err(WebSocketUpgradeRejectionError::MethodNotGet);
        }
        if parts.version < http::Version::HTTP_11 {
            return Err(WebSocketUpgradeRejectionError::InvalidHttpVersion);
        }

        if !header_contains(&parts.headers, http::header::CONNECTION, "upgrade") {
            return Err(WebSocketUpgradeRejectionError::InvalidConnectionHeader);
        }

        if !header_eq(&parts.headers, http::header::UPGRADE, "websocket") {
            return Err(WebSocketUpgradeRejectionError::InvalidUpgradeHeader);
        }

        if !header_eq(&parts.headers, http::header::SEC_WEBSOCKET_VERSION, "13") {
            return Err(WebSocketUpgradeRejectionError::InvalidWebSocketVersionHeader);
        }

        let sec_websocket_key = parts
            .headers
            .get(http::header::SEC_WEBSOCKET_KEY)
            .ok_or(WebSocketUpgradeRejectionError::WebSocketKeyHeaderMissing)?
            .clone();

        let on_upgrade = parts
            .extensions
            .remove::<hyper::upgrade::OnUpgrade>()
            .ok_or(WebSocketUpgradeRejectionError::ConnectionNotUpgradable)?;

        let sec_websocket_protocol = parts
            .headers
            .get(http::header::SEC_WEBSOCKET_PROTOCOL)
            .cloned();

        Ok(Self {
            config: Default::default(),
            headers: Headers {
                sec_websocket_key,
                sec_websocket_protocol,
            },
            on_failed_upgrade: DefaultOnFailedUpgrade,
            on_upgrade,
        })
    }
}

#[cfg(test)]
mod websocket_tests {
    use std::net;

    use futures_util::{SinkExt, StreamExt};
    use http::Uri;
    use motore::Service;
    use tokio::net::TcpStream;
    use tokio_tungstenite::{
        tungstenite::{client::IntoClientRequest, ClientRequestBuilder},
        MaybeTlsStream,
    };
    use volo::net::Address;

    use super::*;
    use crate::{
        server::{
            route::{get, Route},
            test_helpers::empty_cx,
        },
        Router, Server,
    };

    async fn run_ws_handler<Fut, C, R>(
        addr: Address,
        handler: C,
        req: R,
    ) -> (WebSocketStream<MaybeTlsStream<TcpStream>>, ServerResponse)
    where
        R: IntoClientRequest + Unpin,
        Fut: Future<Output = ServerResponse> + Send + 'static,
        C: FnOnce(WebSocketUpgrade) -> Fut + Send + Sync + Clone + 'static,
    {
        let app = Router::new().route("/echo", get(handler));

        tokio::spawn(async move {
            Server::new(app).run(addr).await.unwrap();
        });

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let (socket, response) = tokio_tungstenite::connect_async(req).await.unwrap();

        (
            socket,
            response.map(|response| response.unwrap_or_default().into()),
        )
    }

    #[tokio::test]
    async fn reject_unupgradable_requests() {
        let route: Route<Body> = Route::new(get(
            |ws: Result<WebSocketUpgrade, WebSocketUpgradeRejectionError>| {
                let rejection = ws.unwrap_err();
                assert!(matches!(
                    rejection,
                    WebSocketUpgradeRejectionError::ConnectionNotUpgradable,
                ));
                std::future::ready(())
            },
        ));

        let req = http::Request::builder()
            .version(http::Version::HTTP_11)
            .method(http::Method::GET)
            .header("upgrade", "websocket")
            .header("connection", "Upgrade")
            .header("sec-websocket-key", "6D69KGBOr4Re+Nj6zx9aQA==")
            .header("sec-websocket-version", "13")
            .body(Body::empty())
            .unwrap();

        let mut cx = empty_cx();

        let resp = route.call(&mut cx, req).await.unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
    }

    #[tokio::test]
    async fn reject_non_get_requests() {
        let route: Route<Body> = Route::new(get(
            |ws: Result<WebSocketUpgrade, WebSocketUpgradeRejectionError>| {
                let rejection = ws.unwrap_err();
                assert!(matches!(
                    rejection,
                    WebSocketUpgradeRejectionError::MethodNotGet,
                ));
                std::future::ready(())
            },
        ));

        let req = http::Request::builder()
            .method(http::Method::POST)
            .body(Body::empty())
            .unwrap();

        let mut cx = empty_cx();

        let resp = route.call(&mut cx, req).await.unwrap();

        assert_eq!(resp.status(), http::StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn success_on_upgrade() {
        async fn handle_socket(mut socket: WebSocket) {
            while let Some(Ok(msg)) = socket.next().await {
                match msg {
                    Message::Text(_)
                    | Message::Binary(_)
                    | Message::Close(_)
                    | Message::Frame(_) => {
                        if socket.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Message::Ping(_) | Message::Pong(_) => {}
                }
            }
        }

        let addr = Address::Ip(net::SocketAddr::new(
            net::IpAddr::V4(net::Ipv4Addr::new(127, 0, 0, 1)),
            25231,
        ));

        let builder = ClientRequestBuilder::new(
            format!("ws://{}/echo", addr.clone())
                .parse::<Uri>()
                .unwrap(),
        );

        let (mut ws_stream, _response) = run_ws_handler(
            addr.clone(),
            |ws: WebSocketUpgrade| std::future::ready(ws.on_upgrade(handle_socket)),
            builder,
        )
        .await;

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
