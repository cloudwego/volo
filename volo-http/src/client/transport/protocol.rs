use std::{error::Error, str::FromStr, sync::LazyLock};

use http::{
    header,
    uri::{Authority, Uri},
    version::Version,
};
use hyper::client::conn;
use hyper_util::rt::TokioIo;
use motore::service::Service;
use volo::{context::Context, net::conn::Conn};

use super::connector::HttpMakeConnection;
use crate::{
    body::Body,
    context::ClientContext,
    error::{
        client::{bad_version, no_address, request_error, Result},
        ClientError,
    },
    request::Request,
    response::Response,
};

/// Configuration of HTTP/1
#[derive(Default)]
pub(crate) struct ClientConfig {
    #[cfg(feature = "http1")]
    pub h1: Http1Config,
    #[cfg(feature = "http2")]
    pub h2: Http2Config,
}

#[cfg(feature = "http1")]
pub struct Http1Config {
    title_case_headers: bool,
    ignore_invalid_headers_in_responses: bool,
    max_headers: Option<usize>,
}

#[cfg(feature = "http1")]
impl Default for Http1Config {
    fn default() -> Self {
        Self {
            title_case_headers: true,
            ignore_invalid_headers_in_responses: false,
            max_headers: None,
        }
    }
}

#[cfg(feature = "http1")]
impl Http1Config {
    /// Set whether HTTP/1 connections will write header names as title case at
    /// the socket level.
    ///
    /// Default is false.
    pub fn set_title_case_headers(&mut self, title_case_headers: bool) -> &mut Self {
        self.title_case_headers = title_case_headers;
        self
    }

    /// Set whether HTTP/1 connections will silently ignored malformed header lines.
    ///
    /// If this is enabled and a header line does not start with a valid header
    /// name, or does not include a colon at all, the line will be silently ignored
    /// and no error will be reported.
    ///
    /// Default is false.
    pub fn set_ignore_invalid_headers_in_responses(
        &mut self,
        ignore_invalid_headers_in_responses: bool,
    ) -> &mut Self {
        self.ignore_invalid_headers_in_responses = ignore_invalid_headers_in_responses;
        self
    }

    /// Set the maximum number of headers.
    ///
    /// When a response is received, the parser will reserve a buffer to store headers for optimal
    /// performance.
    ///
    /// If client receives more headers than the buffer size, the error "message header too large"
    /// is returned.
    ///
    /// Note that headers is allocated on the stack by default, which has higher performance. After
    /// setting this value, headers will be allocated in heap memory, that is, heap memory
    /// allocation will occur for each response, and there will be a performance drop of about 5%.
    ///
    /// Default is 100.
    pub fn set_max_headers(&mut self, max_headers: usize) -> &mut Self {
        self.max_headers = Some(max_headers);
        self
    }
}

#[cfg(feature = "http2")]
pub struct Http2Config {
    keep_alive_interval: Option<std::time::Duration>,
    keep_alive_timeout: std::time::Duration,
    keep_alive_while_idle: bool,
}

#[cfg(feature = "http2")]
impl Default for Http2Config {
    fn default() -> Self {
        Self {
            keep_alive_interval: None,
            keep_alive_timeout: std::time::Duration::from_secs(20),
            keep_alive_while_idle: false,
        }
    }
}

#[cfg(feature = "http2")]
impl Http2Config {
    /// Sets an interval for HTTP2 Ping frames should be sent to keep a
    /// connection alive.
    ///
    /// Pass `None` to disable HTTP2 keep-alive.
    ///
    /// Default is currently disabled.
    pub fn set_keep_alive_interval<D>(&mut self, interval: D) -> &mut Self
    where
        D: Into<Option<std::time::Duration>>,
    {
        self.keep_alive_interval = interval.into();
        self
    }

    /// Sets a timeout for receiving an acknowledgement of the keep-alive ping.
    ///
    /// If the ping is not acknowledged within the timeout, the connection will
    /// be closed. Does nothing if `keep_alive_interval` is disabled.
    ///
    /// Default is 20 seconds.
    pub fn set_keep_alive_timeout(&mut self, timeout: std::time::Duration) -> &mut Self {
        self.keep_alive_timeout = timeout;
        self
    }

    /// Sets whether HTTP2 keep-alive should apply while the connection is idle.
    ///
    /// If disabled, keep-alive pings are only sent while there are open
    /// request/responses streams. If enabled, pings are also sent when no
    /// streams are active. Does nothing if `keep_alive_interval` is
    /// disabled.
    ///
    /// Default is `false`.
    pub fn set_keep_alive_while_idle(&mut self, enabled: bool) -> &mut Self {
        self.keep_alive_while_idle = enabled;
        self
    }
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

pub struct ClientTransport {
    #[cfg(feature = "http1")]
    h1_client: conn::http1::Builder,
    #[cfg(feature = "http2")]
    h2_client: conn::http2::Builder<hyper_util::rt::TokioExecutor>,
    config: ClientTransportConfig,
    connector: HttpMakeConnection,
}

#[cfg(feature = "http1")]
fn http1_client(config: &Http1Config) -> conn::http1::Builder {
    let mut builder = conn::http1::Builder::new();
    builder
        .title_case_headers(config.title_case_headers)
        .ignore_invalid_headers_in_responses(config.ignore_invalid_headers_in_responses);
    if let Some(max_headers) = config.max_headers {
        builder.max_headers(max_headers);
    }
    builder
}

#[cfg(feature = "http2")]
fn http2_client(config: &Http2Config) -> conn::http2::Builder<hyper_util::rt::TokioExecutor> {
    let mut builder = conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new());
    builder
        .keep_alive_interval(config.keep_alive_interval)
        .keep_alive_timeout(config.keep_alive_timeout)
        .keep_alive_while_idle(config.keep_alive_while_idle);
    builder
}

impl ClientTransport {
    pub(crate) fn new(
        http_config: ClientConfig,
        transport_config: ClientTransportConfig,
        #[cfg(feature = "__tls")] tls_connector: Option<volo::net::tls::TlsConnector>,
    ) -> Self {
        #[cfg(feature = "http1")]
        let h1_client = http1_client(&http_config.h1);
        #[cfg(feature = "http2")]
        let h2_client = http2_client(&http_config.h2);

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
        }
    }

    async fn handshake<B>(&self, _ver: Version, conn: Conn) -> Result<Connection<B>>
    where
        B: http_body::Body + Unpin + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn Error + Send + Sync>> + 'static,
    {
        let conn = TokioIo::new(conn);

        #[cfg(feature = "http2")]
        {
            #[cfg(feature = "__tls")]
            let use_h2 = match conn.inner().stream.negotiated_alpn().as_deref() {
                Some(alpn) => alpn == b"h2",
                None => true,
            };
            #[cfg(not(feature = "__tls"))]
            let use_h2 = true;

            // 1. Not using TLS or ALPN negotiated to use H2
            // 2. Request specified using H2 or H1 is disabled
            if use_h2 && (_ver == Version::HTTP_2 || cfg!(not(feature = "http1"))) {
                let (sender, conn) = self
                    .h2_client
                    .handshake(conn)
                    .await
                    .map_err(request_error)?;
                tokio::spawn(conn);
                return Ok(Connection::H2(sender));
            }
        }

        #[cfg(feature = "http1")]
        {
            let (sender, conn) = self
                .h1_client
                .handshake(conn)
                .await
                .map_err(request_error)?;
            tokio::spawn(conn);
            return Ok(Connection::H1(sender));
        }

        #[allow(unreachable_code)]
        Err(bad_version())
    }
}

enum Connection<B> {
    #[cfg(feature = "http1")]
    H1(conn::http1::SendRequest<B>),
    #[cfg(feature = "http2")]
    H2(conn::http2::SendRequest<B>),
}

impl<B> Connection<B>
where
    B: http_body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn Error + Send + Sync>> + 'static,
{
    async fn send_request(&mut self, req: Request<B>) -> Result<Response, ClientError> {
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
    let scheme = cx.scheme().to_owned();
    let authority = gen_authority(req);
    let mut parts = req.uri().to_owned().into_parts();
    parts.scheme = Some(scheme);
    parts.authority = Some(authority);
    let Ok(uri) = Uri::from_parts(parts) else {
        return;
    };
    *req.uri_mut() = uri;
}

impl<B> Service<ClientContext, Request<B>> for ClientTransport
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
        let stat_enabled = self.config.stat_enable;
        let addr = cx.rpc_info().callee().address().ok_or_else(no_address)?;
        rewrite_uri(cx, &mut req);

        if stat_enabled {
            cx.stats.record_transport_start_at();
        }

        let conn = self.connector.call(cx, addr).await?;
        let mut conn = self.handshake(req.version(), conn).await?;
        let res = conn.send_request(req).await;

        if stat_enabled {
            cx.stats.record_transport_end_at();
        }

        res
    }
}
