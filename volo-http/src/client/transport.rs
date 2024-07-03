use std::error::Error;

use http_body::Body;
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use motore::{make::MakeConnection, service::Service};
#[cfg(feature = "__tls")]
use volo::net::tls::Connector;
use volo::{
    context::Context,
    net::{conn::Conn, dial::DefaultMakeTransport, Address},
};

use crate::{
    context::ClientContext,
    error::client::{no_address, request_error, ClientError},
    request::ClientRequest,
    response::ClientResponse,
};

/// TLS transport tag with no content
///
/// When implementing a service discover and TLS should be enable, just inserting it to the callee.
///
/// The struct is used for advanced users.
#[cfg(feature = "__tls")]
#[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
pub struct TlsTransport;

#[derive(Clone)]
pub struct ClientTransport {
    client: http1::Builder,
    mk_conn: DefaultMakeTransport,
    config: ClientTransportConfig,
    #[cfg(feature = "__tls")]
    tls_connector: volo::net::tls::TlsConnector,
}

impl ClientTransport {
    pub(super) fn new(
        http_config: ClientConfig,
        transport_config: ClientTransportConfig,
        mk_conn: DefaultMakeTransport,
        #[cfg(feature = "__tls")] tls_connector: volo::net::tls::TlsConnector,
    ) -> Self {
        let mut builder = http1::Builder::new();
        builder
            .title_case_headers(http_config.title_case_headers)
            .preserve_header_case(http_config.preserve_header_case);
        if let Some(max_headers) = http_config.max_headers {
            builder.max_headers(max_headers);
        }

        Self {
            client: builder,
            mk_conn,
            config: transport_config,
            #[cfg(feature = "__tls")]
            tls_connector,
        }
    }

    async fn connect_to(&self, address: Address) -> Result<Conn, ClientError> {
        self.mk_conn.make_connection(address).await.map_err(|err| {
            tracing::error!("[Volo-HTTP] failed to make connection, error: {err}");
            request_error(err)
        })
    }

    #[cfg(feature = "__tls")]
    async fn make_connection(&self, cx: &ClientContext) -> Result<Conn, ClientError> {
        use crate::error::client::bad_scheme;

        let callee = cx.rpc_info().callee();
        let https = callee.contains::<TlsTransport>();

        if self.config.disable_tls && https {
            // TLS is disabled but the request still use TLS
            return Err(bad_scheme());
        }

        let target_addr = callee.address().ok_or_else(no_address)?;
        tracing::debug!("[Volo-HTTP] connecting to target: {target_addr:?}");
        let conn = self.connect_to(target_addr).await;
        if !https {
            // The request does not use TLS, just return it without TLS handshake
            return conn;
        }
        let conn = conn?;

        let target_name = callee.service_name_ref();
        tracing::debug!("[Volo-HTTP] try to make tls handshake, name: {target_name:?}");
        let tcp_stream = match conn.stream {
            volo::net::conn::ConnStream::Tcp(tcp_stream) => tcp_stream,
            _ => unreachable!(),
        };
        println!("target_name: {target_name}");
        self.tls_connector
            .connect(target_name, tcp_stream)
            .await
            .map_err(|err| {
                tracing::error!("[Volo-HTTP] failed to make tls connection, error: {err}");
                request_error(err)
            })
    }

    #[cfg(not(feature = "__tls"))]
    async fn make_connection(&self, cx: &ClientContext) -> Result<Conn, ClientError> {
        let target_addr = cx.rpc_info().callee().address().ok_or_else(no_address)?;
        tracing::debug!("[Volo-HTTP] connecting to target: {target_addr:?}");
        self.connect_to(target_addr).await
    }

    async fn request<B>(
        &self,
        cx: &ClientContext,
        req: ClientRequest<B>,
    ) -> Result<ClientResponse, ClientError>
    where
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn Error + Send + Sync>> + 'static,
    {
        tracing::trace!("[Volo-HTTP] requesting {}", req.uri());
        let conn = self.make_connection(cx).await?;
        let io = TokioIo::new(conn);
        let (mut sender, conn) = self.client.handshake(io).await.map_err(|err| {
            tracing::error!("[Volo-HTTP] failed to handshake, error: {err}");
            request_error(err)
        })?;
        tokio::spawn(conn);
        let resp = sender.send_request(req).await.map_err(|err| {
            tracing::error!("[Volo-HTTP] failed to send request, error: {err}");
            request_error(err)
        })?;
        Ok(resp)
    }
}

impl<B> Service<ClientContext, ClientRequest<B>> for ClientTransport
where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn Error + Send + Sync>> + 'static,
{
    type Response = ClientResponse;
    type Error = ClientError;

    async fn call(
        &self,
        cx: &mut ClientContext,
        req: ClientRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        let stat_enabled = self.config.stat_enable;

        if stat_enabled {
            cx.stats.record_transport_start_at();
        }

        let res = self.request(cx, req).await;

        if stat_enabled {
            cx.stats.record_transport_end_at();
        }

        res
    }
}

pub struct ClientConfig {
    pub title_case_headers: bool,
    pub preserve_header_case: bool,
    pub max_headers: Option<usize>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientConfig {
    pub fn new() -> Self {
        Self {
            title_case_headers: false,
            preserve_header_case: false,
            max_headers: None,
        }
    }
}

#[derive(Clone)]
pub(super) struct ClientTransportConfig {
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
