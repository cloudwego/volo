use std::error::Error;

use http_body::Body;
use motore::service::Service;
use volo::context::Context;

use crate::{
    context::ClientContext,
    error::client::{status_error, ClientError},
    request::ClientRequest,
    response::ClientResponse,
};

#[derive(Clone)]
pub struct MetaService<S> {
    inner: S,
}

impl<S> MetaService<S> {
    pub(super) fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S, B> Service<ClientContext, ClientRequest<B>> for MetaService<S>
where
    S: Service<ClientContext, ClientRequest<B>, Response = ClientResponse, Error = ClientError>
        + Send
        + Sync
        + 'static,
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn Error + Send + Sync>> + 'static,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ClientContext,
        req: ClientRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        let config = cx.rpc_info().config().to_owned();
        let fut = self.inner.call(cx, req);
        let res = match config.timeout {
            Some(duration) => {
                let sleep = tokio::time::sleep(duration);
                tokio::select! {
                    res = fut => res,
                    _ = sleep => {
                        tracing::error!("[Volo-HTTP]: request timeout.");
                        return Err(crate::error::client::timeout());
                    }
                }
            }
            None => fut.await,
        };

        if !config.fail_on_error_status {
            return res;
        }

        let resp = res?;

        let status = resp.status();
        if status.is_client_error() || status.is_server_error() {
            Err(status_error(status))
        } else {
            Ok(resp)
        }
    }
}
