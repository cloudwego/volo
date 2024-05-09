//! This is a copy of `volo::loadbalance::layer` without the retry logic. Because retry needs the
//! `Req` has `Clone` trait, but HTTP body may be a stream, which cannot be cloned. So we remove
//! the retry related codes here.
//!
//! In addition, HTTP service can use DNS as service discover, so the default load balance uses a
//! DNS resolver for pick a target address (the DNS resolver picks only one because it does not
//! need load balance).

use std::{fmt::Debug, sync::Arc};

use motore::{layer::Layer, service::Service};
use volo::{
    context::Context,
    discovery::Discover,
    loadbalance::{random::WeightedRandomBalance, LoadBalance, MkLbLayer},
};

use super::dns_discover::DnsResolver;
use crate::{
    context::ClientContext,
    error::{
        client::{lb_error, no_available_endpoint},
        ClientError,
    },
    request::ClientRequest,
    response::ClientResponse,
};

pub type DefaultLB = LbConfig<WeightedRandomBalance<<DnsResolver as Discover>::Key>, DnsResolver>;
pub type DefaultLBService<S> = LoadBalanceService<DnsResolver, WeightedRandomBalance<()>, S>;

pub struct LbConfig<L, DISC> {
    load_balance: L,
    discover: DISC,
}

impl Default for DefaultLB {
    fn default() -> Self {
        LbConfig::new(WeightedRandomBalance::new(), DnsResolver)
    }
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
                        Err(err) => {
                            tracing::warn!("[VOLO] discovering subscription error: {:?}", err)
                        }
                    }
                }
            });
        }
        service
    }
}

impl<D, LB, S, B> Service<ClientContext, ClientRequest<B>> for LoadBalanceService<D, LB, S>
where
    D: Discover,
    LB: LoadBalance<D>,
    S: Service<ClientContext, ClientRequest<B>, Response = ClientResponse, Error = ClientError>
        + Send
        + Sync,
    B: Send,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ClientContext,
        req: ClientRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        let callee = cx.rpc_info().callee();

        let mut picker = match &callee.address {
            None => self
                .load_balance
                .get_picker(callee, &self.discover)
                .await
                .map_err(lb_error)?,
            _ => {
                return self.service.call(cx, req).await;
            }
        };

        let addr = picker.next().ok_or_else(no_available_endpoint)?;
        cx.rpc_info_mut().callee_mut().set_address(addr);

        self.service.call(cx, req).await
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
