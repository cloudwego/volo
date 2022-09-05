use std::{fmt::Debug, future::Future, sync::Arc};

use anyhow::{anyhow, Context as _};
use motore::{BoxError, Service};
use tracing::warn;
use volo::{
    context::Context,
    discovery::Discover,
    loadbalance::{LoadBalance, MkLbLayer},
    Layer,
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

        if let Some(mut channel) = service.discover.watch() {
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
    <Cx as Context>::Config: std::marker::Sync,
    Cx: 'static + Context + Send + Sync,
    D: Discover,
    LB: LoadBalance<D>,
    S: Service<Cx, Request<T>> + 'static + Send,
    S::Error: Into<BoxError> + Debug,
    T: Send + 'static,
{
    type Response = S::Response;

    type Error = BoxError;

    type Future<'cx> = impl Future<Output = Result<Self::Response, Self::Error>> + Send + 'cx
    where
        Self: 'cx;

    fn call<'cx, 's>(&'s mut self, cx: &'cx mut Cx, req: Request<T>) -> Self::Future<'cx>
    where
        's: 'cx,
    {
        debug_assert!(
            cx.rpc_info().callee.is_some(),
            "must set callee endpoint before load balance service"
        );
        async move {
            if let Some(info) = &cx.rpc_info().callee {
                let mut picker = match &info.address {
                    None => self
                        .load_balance
                        .get_picker(info, &self.discover)
                        .await
                        .context("discover instance error")?,
                    _ => {
                        return self.service.call(cx, req).await.map_err(Into::into);
                    }
                };

                if let Some(addr) = picker.next() {
                    if let Some(callee) = cx.rpc_info_mut().callee_mut() {
                        callee.address = Some(addr.clone())
                    }

                    match self.service.call(cx, req).await {
                        Ok(resp) => {
                            return Ok(resp);
                        }
                        Err(err) => {
                            tracing::warn!("[VOLO] call endpoint: {:?} error: {:?}", addr, err);
                        }
                    }
                } else {
                    tracing::warn!("[VOLO] zero call count, call info: {:?}", cx.rpc_info());
                }
                Err(anyhow!("load balance call reaches end").into())
            } else {
                Err(anyhow!("load balance get empty endpoint").into())
            }
        }
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

impl<LB, DISC, S> MkLbLayer<S> for LbConfig<LB, DISC> {
    type Layer = LoadBalanceLayer<DISC, LB>;

    fn make(self) -> Self::Layer {
        LoadBalanceLayer::new(self.discover, self.load_balance)
    }
}
