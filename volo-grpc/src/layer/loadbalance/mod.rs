use std::{fmt::Debug, sync::Arc};

use motore::Service;
use tracing::warn;
use volo::{
    context::Context,
    discovery::Discover,
    loadbalance::{error::LoadBalanceError, LoadBalance, MkLbLayer},
    Layer, Unwrap,
};

use crate::Request;

#[derive(Clone, Default, Copy)]
pub struct LoadBalanceLayer<D, LB> {
    discover: D,
    load_balance: LB,
}

impl<D, LB> LoadBalanceLayer<D, LB> {
    pub fn new(discover: D, load_balance: LB) -> Self {
        LoadBalanceLayer {
            discover,
            load_balance,
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
        LoadBalanceService::new(self.discover, self.load_balance, inner)
    }
}
#[derive(Clone)]
pub struct LoadBalanceService<D, LB, S> {
    discover: D,
    load_balance: Arc<LB>,
    service: S,
}

impl<D, LB, S> LoadBalanceService<D, LB, S>
where
    D: Discover,
    LB: LoadBalance<D>,
{
    pub fn new(discover: D, load_balance: LB, service: S) -> Self {
        let lb = Arc::new(load_balance);

        let service = Self {
            discover,
            load_balance: lb.clone(),
            service,
        };

        if let Some(mut channel) = service.discover.watch(None) {
            tokio::spawn(async move {
                loop {
                    match channel.recv().await {
                        Ok(recv) => lb.rebalance(recv),
                        Err(err) => warn!("[VOLO] discovering subscription error {:?}", err),
                    }
                }
            });
        }
        service
    }
}

impl<Cx, T, D, LB, S> Service<Cx, Request<T>> for LoadBalanceService<D, LB, S>
where
    <Cx as Context>::Config: Sync,
    Cx: 'static + Context + Send + Sync,
    D: Discover,
    LB: LoadBalance<D>,
    S: Service<Cx, Request<T>> + 'static + Send + Sync,
    LoadBalanceError: Into<S::Error>,
    S::Error: Debug,
    T: Send + 'static,
{
    type Response = S::Response;

    type Error = S::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut Cx,
        req: Request<T>,
    ) -> Result<Self::Response, Self::Error> {
        debug_assert!(
            cx.rpc_info().callee.is_some(),
            "must set callee endpoint before load balance service"
        );
        let callee = cx.rpc_info().callee().volo_unwrap();

        let mut picker = match &callee.address {
            None => self
                .load_balance
                .get_picker(callee, &self.discover)
                .await
                .map_err(|err| err.into())?,
            _ => {
                return self.service.call(cx, req).await.map_err(Into::into);
            }
        };

        if let Some(addr) = picker.next() {
            if let Some(callee) = cx.rpc_info_mut().callee_mut() {
                callee.address = Some(addr.clone())
            }

            return match self.service.call(cx, req).await {
                Ok(resp) => Ok(resp),
                Err(err) => {
                    warn!("[VOLO] call endpoint: {:?} error: {:?}", addr, err);
                    Err(err)
                }
            };
        } else {
            warn!("[VOLO] zero call count, call info: {:?}", cx.rpc_info());
        }
        Err(LoadBalanceError::Retry).map_err(|err| err.into())?
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

pub struct LbConfig<L, DISC> {
    load_balance: L,
    discover: DISC,
}

impl<L, DISC> LbConfig<L, DISC> {
    pub fn new(load_balance: L, discover: DISC) -> Self {
        LbConfig {
            load_balance,
            discover,
        }
    }

    pub fn load_balance<NL>(self, load_balance: NL) -> LbConfig<NL, DISC> {
        LbConfig {
            load_balance,
            discover: self.discover,
        }
    }

    pub fn discover<NDISC>(self, discover: NDISC) -> LbConfig<L, NDISC> {
        LbConfig {
            load_balance: self.load_balance,
            discover,
        }
    }
}

impl<LB, DISC> MkLbLayer for LbConfig<LB, DISC> {
    type Layer = LoadBalanceLayer<DISC, LB>;

    fn make(self) -> Self::Layer {
        LoadBalanceLayer::new(self.discover, self.load_balance)
    }
}
