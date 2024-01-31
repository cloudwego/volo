use http::{header, HeaderValue};
use motore::service::Service;
use volo::context::Context;

use crate::{
    context::{client::Host, ClientContext},
    error::client::ClientError,
    request::ClientRequest,
    response::ClientResponse,
};

#[derive(Clone)]
pub struct MetaService<S> {
    inner: S,
}

impl<S> MetaService<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S> Service<ClientContext, ClientRequest> for MetaService<S>
where
    S: Service<ClientContext, ClientRequest, Response = ClientResponse, Error = ClientError>
        + Send
        + Sync
        + 'static,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ClientContext,
        mut req: ClientRequest,
    ) -> Result<Self::Response, Self::Error> {
        let config = cx.rpc_info().config();
        let host = match config.host {
            Host::CalleeName => Some(HeaderValue::from_str(
                cx.rpc_info().callee().service_name_ref(),
            )),
            Host::TargetAddress => cx
                .rpc_info()
                .callee()
                .address()
                .map(|addr| HeaderValue::from_str(&format!("{}", addr))),
            Host::None => None,
        };
        if let Some(Ok(val)) = host {
            req.headers_mut().insert(header::HOST, val);
        }
        self.inner.call(cx, req).await
    }
}
