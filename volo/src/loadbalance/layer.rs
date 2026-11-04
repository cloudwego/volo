use std::{fmt::Debug, sync::Arc};

use async_broadcast::RecvError;
use motore::Service;
use tracing::warn;

use super::error::{LoadBalanceError, Retryable};
use crate::{Layer, context::Context, discovery::Discover, loadbalance::LoadBalance};

#[derive(Clone)]
pub struct LoadBalanceService<D, LB, S> {
    state: Arc<SharedState<D, LB>>,
    service: S,
    retry: usize,
}

impl<D, LB, S> LoadBalanceService<D, LB, S>
where
    D: Discover,
    LB: LoadBalance<D>,
{
    pub fn new(discover: D, load_balance: LB, service: S, retry: usize) -> Self {
        Self::from_shared(
            Arc::new(SharedState {
                discover,
                load_balance,
            }),
            service,
            retry,
        )
    }
    fn from_shared(state: Arc<SharedState<D, LB>>, service: S, retry: usize) -> Self {
        let service = Self {
            state,
            service,
            retry,
        };

        if let Some(mut channel) = service.state.discover.watch(None) {
            // The watch task must not keep the discovery producer alive after
            // both unary and streaming owners are gone.
            let state = Arc::downgrade(&service.state);
            tokio::spawn(async move {
                loop {
                    match channel.recv().await {
                        Ok(recv) => match state.upgrade() {
                            Some(state) => state.load_balance.rebalance(recv),
                            None => break,
                        },
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
                .state
                .load_balance
                .get_picker(callee, &self.state.discover)
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
            .field("discover", &self.state.discover)
            .field("load_balancer", &self.state.load_balance)
            .finish()
    }
}

pub(super) struct SharedState<D, LB> {
    pub discover: D,
    pub load_balance: LB,
}

pub struct LoadBalanceLayer<D, LB> {
    pub(super) state: Arc<SharedState<D, LB>>,
    retry_count: usize,
}

impl<D, LB> Clone for LoadBalanceLayer<D, LB> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            retry_count: self.retry_count,
        }
    }
}
impl<D: Default, LB: Default> Default for LoadBalanceLayer<D, LB> {
    fn default() -> Self {
        Self::new(D::default(), LB::default(), 0)
    }
}

impl<D, LB> LoadBalanceLayer<D, LB> {
    pub fn new(discover: D, load_balance: LB, retry_count: usize) -> Self {
        LoadBalanceLayer {
            state: Arc::new(SharedState {
                discover,
                load_balance,
            }),
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
        LoadBalanceService::from_shared(self.state, inner, self.retry_count)
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

#[cfg(test)]
mod shared_state_lifetime_tests {
    use std::{
        convert::Infallible,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use crate::{
        Layer,
        context::Endpoint,
        discovery::{Change, Discover, Instance},
        loadbalance::{LbConfig, LoadBalance, MkLbLayer, error::LoadBalanceError},
        net::Address,
    };
    struct WatchDiscover {
        sender: async_broadcast::Sender<Change<()>>,
        drops: Arc<AtomicUsize>,
    }
    impl Drop for WatchDiscover {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl Discover for WatchDiscover {
        type Key = ();
        type Error = Infallible;
        async fn discover(&self, _: &Endpoint) -> Result<Vec<Arc<Instance>>, Self::Error> {
            Ok(Vec::new())
        }
        fn key(&self, _: &Endpoint) {}
        fn watch(&self, _: Option<&[()]>) -> Option<async_broadcast::Receiver<Change<()>>> {
            Some(self.sender.new_receiver())
        }
    }
    struct Balance(Arc<tokio::sync::Notify>);
    impl LoadBalance<WatchDiscover> for Balance {
        type InstanceIter = std::vec::IntoIter<Address>;
        async fn get_picker(
            &self,
            _: &Endpoint,
            _: &WatchDiscover,
        ) -> Result<Self::InstanceIter, LoadBalanceError> {
            Ok(Vec::new().into_iter())
        }
        fn rebalance(&self, _: Change<()>) {
            self.0.notify_one();
        }
    }
    #[tokio::test]
    async fn watch_uses_shared_state_without_retaining_discovery_after_owners_drop() {
        let (sender, receiver) = async_broadcast::broadcast(1);
        let drops = Arc::new(AtomicUsize::new(0));
        let notified = Arc::new(tokio::sync::Notify::new());
        let (layer, resolver) = LbConfig::new(
            Balance(notified.clone()),
            WatchDiscover {
                sender: sender.clone(),
                drops: drops.clone(),
            },
        )
        .make_with_resolver();
        let weak = Arc::downgrade(&layer.state);
        let service = layer.layer(());
        drop(receiver);
        sender
            .broadcast(Change {
                key: (),
                all: Vec::new(),
                added: Vec::new(),
                updated: Vec::new(),
                removed: Vec::new(),
            })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), notified.notified())
            .await
            .unwrap();
        drop(service);
        assert!(
            weak.upgrade().is_some(),
            "resolver shares the same discovery state"
        );
        drop(resolver);
        assert!(
            weak.upgrade().is_none(),
            "watch task may not keep producer/state alive"
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        drop(sender);
        tokio::task::yield_now().await;
    }
}
