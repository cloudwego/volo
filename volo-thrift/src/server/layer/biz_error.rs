use motore::{layer::Layer, service::Service};

use crate::{ServerError, context::ServerContext};

#[derive(Clone)]
pub struct BizErrorLayer;

impl BizErrorLayer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BizErrorLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for BizErrorLayer {
    type Service = BizErrorService<S>;

    #[inline]
    fn layer(self, inner: S) -> Self::Service {
        BizErrorService { inner }
    }
}

#[derive(Clone)]
pub struct BizErrorService<S> {
    inner: S,
}

impl<S, Req, Resp> Service<ServerContext, Req> for BizErrorService<S>
where
    S: Service<ServerContext, Req, Response = Resp> + Send + 'static + Sync,
    S::Error: Into<crate::ServerError>,
    Req: Send + 'static,
{
    type Response = S::Response;

    type Error = crate::ServerError;

    #[inline]
    async fn call(&self, cx: &mut ServerContext, req: Req) -> Result<Self::Response, Self::Error> {
        let ret = self.inner.call(cx, req).await.map_err(Into::into);
        if let Err(ServerError::Biz(err)) = ret.as_ref() {
            cx.common_stats.set_biz_error(err.clone());
        }
        ret
    }
}
