use std::error::Error;

use http::header;
use http_body::Body;
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use motore::{make::MakeConnection, service::Service};
use volo::{context::Context, net::Address};

use crate::{
    context::ClientContext,
    error::client::{no_address, request_error, ClientError},
    request::ClientRequest,
    response::ClientResponse,
};

pub struct ClientTransport<MkT> {
    client: http1::Builder,
    mk_conn: MkT,
}

impl<MkT> ClientTransport<MkT> {
    pub fn new(_config: ClientConfig, mk_conn: MkT) -> Self {
        Self {
            client: http1::Builder::new(),
            mk_conn,
        }
    }

    async fn request<B>(
        &self,
        target: Address,
        req: ClientRequest<B>,
    ) -> Result<ClientResponse, ClientError>
    where
        MkT: MakeConnection<Address> + Send + Sync,
        MkT::Connection: 'static,
        MkT::Error: Error + Send + Sync + 'static,
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn Error + Send + Sync>> + 'static,
    {
        let conn = self
            .mk_conn
            .make_connection(target)
            .await
            .map_err(request_error)?;
        let io = TokioIo::new(conn);
        let (mut sender, conn) = self.client.handshake(io).await.map_err(request_error)?;
        tokio::spawn(conn);
        let resp = sender.send_request(req).await.map_err(request_error)?;
        Ok(resp)
    }
}

impl<MkT, B> Service<ClientContext, ClientRequest<B>> for ClientTransport<MkT>
where
    MkT: MakeConnection<Address> + Send + Sync,
    MkT::Connection: 'static,
    MkT::Error: Error + Send + Sync + 'static,
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn Error + Send + Sync>> + 'static,
{
    type Response = ClientResponse;
    type Error = ClientError;

    async fn call(
        &self,
        cx: &mut ClientContext,
        mut req: ClientRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        // `Content-Length` must be set here because the body may be changed in previous layer(s).
        if let Some(len) = req.body().size_hint().exact() {
            if req.headers().get(header::CONTENT_LENGTH).is_none() {
                req.headers_mut().insert(header::CONTENT_LENGTH, len.into());
            }
        }

        let target = cx.rpc_info.callee().address().ok_or_else(no_address)?;
        let stat_enable = cx.rpc_info().config().stat_enable;

        if stat_enable {
            if let Some(req_size) = req.size_hint().exact() {
                cx.common_stats.set_req_size(req_size);
            }
            cx.stats.record_transport_start_at();
        }

        let resp = self.request(target, req).await;

        if stat_enable {
            cx.stats.record_transport_end_at();

            if let Ok(response) = resp.as_ref() {
                cx.stats.set_status_code(response.status());
                if let Some(resp_size) = response.size_hint().exact() {
                    cx.common_stats.set_resp_size(resp_size);
                }
            }
        }

        resp
    }
}

#[derive(Default)]
pub struct ClientConfig {}
