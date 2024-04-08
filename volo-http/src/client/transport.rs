use std::error::Error;

use http_body::Body;
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use motore::{make::MakeConnection, service::Service};
#[cfg(feature = "__tls")]
use volo::net::tls::Connector;
use volo::{
    context::Context,
    net::{conn::Conn, dial::DefaultMakeTransport},
};

use crate::{
    context::ClientContext,
    error::client::{no_address, request_error, ClientError},
    request::ClientRequest,
    response::ClientResponse,
};

#[derive(Clone)]
pub struct ClientTransport {
    client: http1::Builder,
    mk_conn: DefaultMakeTransport,
    #[cfg(feature = "__tls")]
    tls_connector: volo::net::tls::TlsConnector,
}

impl ClientTransport {
    pub fn new(
        config: ClientConfig,
        mk_conn: DefaultMakeTransport,
        #[cfg(feature = "__tls")] tls_connector: volo::net::tls::TlsConnector,
    ) -> Self {
        let mut builder = http1::Builder::new();
        builder
            .title_case_headers(config.title_case_headers)
            .preserve_header_case(config.preserve_header_case);
        if let Some(max_headers) = config.max_headers {
            builder.max_headers(max_headers);
        }

        Self {
            client: builder,
            mk_conn,
            #[cfg(feature = "__tls")]
            tls_connector,
        }
    }

    #[cfg(feature = "__tls")]
    async fn make_connection(&self, cx: &ClientContext) -> Result<Conn, ClientError> {
        let target_addr = cx.rpc_info().callee().address().ok_or_else(no_address)?;
        match target_addr {
            volo::net::Address::Ip(_) if cx.is_tls() => {
                let target_name = cx.rpc_info().callee().service_name_ref();
                tracing::debug!("connecting to tls target: {target_addr:?}, name: {target_name:?}");
                let conn = self
                    .mk_conn
                    .make_connection(target_addr)
                    .await
                    .map_err(|err| {
                        tracing::warn!("failed to make connection, error: {err}");
                        request_error(err)
                    })?;
                let tcp_stream = match conn.stream {
                    volo::net::conn::ConnStream::Tcp(tcp_stream) => tcp_stream,
                    _ => unreachable!(),
                };
                self.tls_connector
                    .connect(target_name, tcp_stream)
                    .await
                    .map_err(|err| {
                        tracing::warn!("failed to make tls connection, error: {err}");
                        request_error(err)
                    })
            }
            _ => {
                tracing::debug!("fallback to non-tls target: {target_addr:?}");
                self.mk_conn
                    .make_connection(target_addr)
                    .await
                    .map_err(|err| {
                        tracing::warn!("failed to make connection, error: {err}");
                        request_error(err)
                    })
            }
        }
    }

    #[cfg(not(feature = "__tls"))]
    async fn make_connection(&self, cx: &ClientContext) -> Result<Conn, ClientError> {
        let target_addr = cx.rpc_info().callee().address().ok_or_else(no_address)?;
        tracing::debug!("connecting to target: {target_addr:?}");
        self.mk_conn
            .make_connection(target_addr)
            .await
            .map_err(|err| {
                tracing::warn!("failed to make connection, error: {err}");
                request_error(err)
            })
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
        let conn = self.make_connection(cx).await?;
        let io = TokioIo::new(conn);
        let (mut sender, conn) = self.client.handshake(io).await.map_err(|err| {
            tracing::warn!("failed to handshake, error: {err}");
            request_error(err)
        })?;
        tokio::spawn(conn);
        let resp = sender.send_request(req).await.map_err(|err| {
            tracing::warn!("failed to send request, error: {err}");
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
        let stat_enabled = cx.stat_enabled();

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
