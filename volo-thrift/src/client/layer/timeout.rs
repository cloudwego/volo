//! Applies a timeout to request
//! if the inner service's call does not complete within specified timeout, the response will be
//! aborted.
use motore::{layer::Layer, service::Service};
use tracing::warn;

use crate::context::ClientContext;

#[derive(Clone)]
pub struct Timeout<S> {
    inner: S,
}

impl<Req, S> Service<ClientContext, Req> for Timeout<S>
where
    Req: 'static + Send,
    S: Service<ClientContext, Req, Error = crate::ClientError> + 'static + Send + Sync,
{
    type Response = S::Response;

    type Error = S::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut ClientContext,
        req: Req,
    ) -> Result<Self::Response, Self::Error> {
        match cx.rpc_info.config().rpc_timeout() {
            Some(duration) => {
                let start = std::time::Instant::now();
                match tokio::time::timeout(duration, self.inner.call(cx, req)).await {
                    Ok(r) => r.map_err(Into::into),
                    Err(_) => {
                        let msg = format!(
                            "[VOLO] thrift rpc call timeout, rpcinfo: {:?}, elpased: {:?}, \
                             timeout config: {:?}",
                            cx.rpc_info,
                            start.elapsed(),
                            duration
                        );
                        warn!(msg);
                        Err(crate::ApplicationException::new(
                            crate::ApplicationExceptionKind::INTERNAL_ERROR,
                            msg,
                        )
                        .into())
                    }
                }
            }
            None => self.inner.call(cx, req).await.map_err(Into::into),
        }
    }
}

#[derive(Clone, Default, Copy)]
pub struct TimeoutLayer;

impl TimeoutLayer {
    #[allow(dead_code)]
    pub fn new() -> Self {
        TimeoutLayer
    }
}

impl<S> Layer<S> for TimeoutLayer {
    type Service = Timeout<S>;

    fn layer(self, inner: S) -> Self::Service {
        Timeout { inner }
    }
}
