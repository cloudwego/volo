//! Applies a timeout to request
//! if the inner service's call does not complete within specified timeout, the response will be
//! aborted.

use futures::Future;
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
                        let start = std::time::Instant::now();
                        match tokio::time::timeout(duration, self.inner.call(cx, req)).await {
                            Ok(r) => r.map_err(Into::into),
                            Err(_) => {
                                let msg = format!(
                                    "[VOLO] thrift rpc call timeout, rpcinfo: {:?}, elpased: \
                                     {:?}, timeout config: {:?}",
                                    cx.rpc_info,
                                    start.elapsed(),
                                    duration
                                );
                                warn!(msg);
                                Err(crate::Error::Transport(
                                    std::io::Error::new(std::io::ErrorKind::TimedOut, msg).into(),
                                ))
                            }
                        }
                    }
                    None => self.inner.call(cx, req).await.map_err(Into::into),
                }
            } else {
                unreachable!("rpc_info.config should never be None")
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
