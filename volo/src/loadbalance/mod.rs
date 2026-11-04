pub mod consistent_hash;
pub mod error;
mod layer;
pub mod random;

use std::{future::Future, pin::Pin, sync::Arc};

use self::{error::LoadBalanceError, layer::LoadBalanceLayer};
use crate::{
    context::Endpoint,
    discovery::{Change, Discover},
    net::Address,
};

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct RequestHash(pub u64);

/// [`LoadBalance`] promise the feature of the load balance policy.
pub trait LoadBalance<D>: Send + Sync + 'static
where
    D: Discover,
{
    /// `InstanceIter` is an iterator of [`crate::discovery::Instance`].
    type InstanceIter: Iterator<Item = Address> + Send;

    /// `get_picker` allows to get an instance iterator of a specified endpoint from self or
    /// service discovery.
    fn get_picker<'future>(
        &'future self,
        endpoint: &'future Endpoint,
        discover: &'future D,
    ) -> impl Future<Output = Result<Self::InstanceIter, LoadBalanceError>> + Send;
    /// `rebalance` is the callback method be used in service discovering subscription.
    fn rebalance(&self, changes: Change<D::Key>);
}

pub trait MkLbLayer {
    type Layer;

    fn make(self) -> Self::Layer;

    /// Supplies a shared endpoint resolver for additional protocol entry points.
    /// Custom unary layers keep their original behavior and may opt in explicitly.
    fn make_with_resolver(self) -> (Self::Layer, Option<EndpointResolver>)
    where
        Self: Sized,
    {
        (self.make(), None)
    }
}

type ResolveFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Address, LoadBalanceError>> + Send + 'a>>;
trait ResolveEndpoint: Send + Sync {
    fn resolve<'a>(&'a self, endpoint: &'a Endpoint) -> ResolveFuture<'a>;
}
/// Selects an address using the same discovery and balancing state as unary calls.
/// This resolves once; it never replays a streaming request.
#[derive(Clone)]
pub struct EndpointResolver(Arc<dyn ResolveEndpoint>);
impl EndpointResolver {
    pub async fn resolve(&self, endpoint: &Endpoint) -> Result<Address, LoadBalanceError> {
        self.0.resolve(endpoint).await
    }
}
struct SharedResolver<D, LB> {
    state: Arc<layer::SharedState<D, LB>>,
}
impl<D: Discover, LB: LoadBalance<D>> ResolveEndpoint for SharedResolver<D, LB> {
    fn resolve<'a>(&'a self, endpoint: &'a Endpoint) -> ResolveFuture<'a> {
        Box::pin(async move {
            if let Some(address) = endpoint.address() {
                return Ok(address);
            }
            self.state
                .load_balance
                .get_picker(endpoint, &self.state.discover)
                .await?
                .next()
                .ok_or(LoadBalanceError::Retry)
        })
    }
}

pub struct LbConfig<L, DISC> {
    load_balance: L,
    discover: DISC,
    retry_count: usize,
}

impl<L, DISC> LbConfig<L, DISC> {
    pub fn new(load_balance: L, discover: DISC) -> Self {
        LbConfig {
            load_balance,
            discover,
            retry_count: 0,
        }
    }

    pub fn load_balance<NL>(self, load_balance: NL) -> LbConfig<NL, DISC> {
        LbConfig {
            load_balance,
            discover: self.discover,
            retry_count: self.retry_count,
        }
    }

    pub fn discover<NDISC>(self, discover: NDISC) -> LbConfig<L, NDISC> {
        LbConfig {
            load_balance: self.load_balance,
            discover,
            retry_count: self.retry_count,
        }
    }

    /// Sets the retry count of the client.
    pub fn retry_count(mut self, count: usize) -> Self {
        self.retry_count = count;
        self
    }
}

pub struct CustomLayer<L>(pub L);

impl<LB: LoadBalance<DISC>, DISC: Discover> MkLbLayer for LbConfig<LB, DISC> {
    type Layer = LoadBalanceLayer<DISC, LB>;

    fn make(self) -> Self::Layer {
        LoadBalanceLayer::new(self.discover, self.load_balance, self.retry_count)
    }
    fn make_with_resolver(self) -> (Self::Layer, Option<EndpointResolver>) {
        let layer = self.make();
        let resolver = EndpointResolver(Arc::new(SharedResolver {
            state: layer.state.clone(),
        }));
        (layer, Some(resolver))
    }
}

impl<L> MkLbLayer for CustomLayer<L> {
    type Layer = L;

    fn make(self) -> Self::Layer {
        self.0
    }
}
