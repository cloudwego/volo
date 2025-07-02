use std::{fmt::Debug, sync::Arc};

use async_broadcast::RecvError;
use motore::Service;
use tracing::warn;

use super::error::{LoadBalanceError, Retryable};
use crate::{Layer, context::Context, discovery::Discover, loadbalance::LoadBalance};

#[derive(Clone)]
pub struct LoadBalanceService<D, LB, S> {
    discover: D,
    load_balance: Arc<LB>,
    service: S,
    retry: usize,
}

impl<D, LB, S> LoadBalanceService<D, LB, S>
where
    D: Discover,
    LB: LoadBalance<D>,
{
    pub fn new(discover: D, load_balance: LB, service: S, retry: usize) -> Self {
        let lb = Arc::new(load_balance);

        let service = Self {
            discover,
            load_balance: lb.clone(),
            service,
            retry,
        };

        if let Some(mut channel) = service.discover.watch(None) {
            tokio::spawn(async move {
                loop {
                    match channel.recv().await {
                        Ok(recv) => lb.rebalance(recv),
                        Err(err) => match err {
                            RecvError::Closed => break,
                            _ => warn!("[VOLO] discovering subscription error: {:?}", err),
                        },
                    }
                }
            });
        }
        service
    }
}

impl<Cx, Req, D, LB, S> Service<Cx, Req> for LoadBalanceService<D, LB, S>
where
    Cx: 'static + Context + Send + Sync,
    D: Discover,
    LB: LoadBalance<D>,
    S: Service<Cx, Req> + 'static + Send + Sync,
    LoadBalanceError: Into<S::Error>,
    S::Error: Debug + Retryable,
    Req: Clone + Send + Sync + 'static,
{
    type Response = S::Response;

    type Error = S::Error;

    async fn call(&self, cx: &mut Cx, req: Req) -> Result<Self::Response, Self::Error> {
        let callee = cx.rpc_info().callee();

        let picker = match &callee.address {
            None => self
                .load_balance
                .get_picker(callee, &self.discover)
                .await
                .map_err(|err| err.into())?,
            _ => {
                return self.service.call(cx, req).await;
            }
        };
        let mut call_count = 0;
        for (addr, _) in picker.zip(0..self.retry + 1) {
            call_count += 1;
            cx.rpc_info_mut().callee_mut().address = Some(addr.clone());

            match self.service.call(cx, req.clone()).await {
                Ok(resp) => {
                    return Ok(resp);
                }
                Err(err) => {
                    warn!("[VOLO] call rpcinfo: {:?}, error: {:?}", cx.rpc_info(), err);
                    if !err.retryable() {
                        return Err(err);
                    }
                }
            }
        }
        if call_count == 0 {
            warn!("[VOLO] zero call count, call rpcinfo: {:?}", cx.rpc_info());
        }
        Err(LoadBalanceError::Retry.into())
    }
}

impl<D, LB, S> Debug for LoadBalanceService<D, LB, S>
where
    D: Debug,
    LB: Debug,
    S: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LBService")
            .field("discover", &self.discover)
            .field("load_balancer", &self.load_balance)
            .finish()
    }
}

#[derive(Clone, Default, Copy)]
pub struct LoadBalanceLayer<D, LB> {
    discover: D,
    load_balance: LB,
    retry_count: usize,
}

impl<D, LB> LoadBalanceLayer<D, LB> {
    pub fn new(discover: D, load_balance: LB, retry_count: usize) -> Self {
        LoadBalanceLayer {
            discover,
            load_balance,
            retry_count,
        }
    }
}

impl<D, LB, S> Layer<S> for LoadBalanceLayer<D, LB>
where
    D: Discover,
    LB: LoadBalance<D>,
{
    type Service = LoadBalanceService<D, LB, S>;

    fn layer(self, inner: S) -> Self::Service {
        LoadBalanceService::new(self.discover, self.load_balance, inner, self.retry_count)
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use motore::service::service_fn;

    use super::LoadBalanceService;
    use crate::{discovery::StaticDiscover, loadbalance::random::WeightedRandomBalance};

    #[derive(Debug)]
    struct MotoreContext;

    async fn handle(cx: &mut MotoreContext, request: String) -> Result<String, Infallible> {
        println!("{cx:?}, {request:?}");
        Ok::<_, Infallible>(request.to_uppercase())
    }

    #[test]
    fn test_service() {
        let discover = StaticDiscover::from(vec!["127.0.0.1:8000".parse().unwrap()]);
        let lb = WeightedRandomBalance::with_discover(&discover);
        let service = service_fn(handle);

        LoadBalanceService::new(discover, lb, service, 1);
    }
}
