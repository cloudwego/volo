//! Applies a timeout to request
//! if the inner service's call does not complete within specified timeout, the response will be
//! aborted.

use futures::Future;
use motore::{layer::Layer, service::Service};

use crate::context::ClientContext;

#[derive(Clone)]
pub struct Timeout<S> {
    inner: S,
}

impl<Req, S> Service<ClientContext, Req> for Timeout<S>
where
    Req: 'static + Send,
    S: Service<ClientContext, Req> + 'static + Send + Sync,
    S::Error: Send + Sync + Into<crate::Error>,
{
    type Response = S::Response;

    type Error = crate::Error;

    type Future<'cx> = impl Future<Output = Result<S::Response, Self::Error>> + 'cx;

    fn call<'cx, 's>(&'s self, cx: &'cx mut ClientContext, req: Req) -> Self::Future<'cx>
    where
        's: 'cx,
    {
        async move {
            if let Some(config) = cx.rpc_info.config() {
                match config.rpc_timeout() {
                    Some(duration) => {
                        let sleep = tokio::time::sleep(duration);
                        tokio::select! {
                            r = self.inner.call(cx, req) => {
                                r.map_err(Into::into)
                            },
                            _ = sleep => Err(crate::Error::Pilota(std::io::Error::new(std::io::ErrorKind::TimedOut, "service time out").into())),
                        }
                    }
                    None => self.inner.call(cx, req).await.map_err(Into::into),
                }
            } else {
                self.inner.call(cx, req).await.map_err(Into::into)
            }
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
