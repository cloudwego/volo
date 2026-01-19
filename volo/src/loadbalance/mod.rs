pub mod adaptive;
pub mod consistent_hash;
pub mod error;
mod layer;
pub mod least_conn;
pub mod p2c;
pub mod random;
pub mod response_time_weighted;
pub mod round_robin;
#[macro_use]
mod macros;


use self::{error::LoadBalanceError, layer::LoadBalanceLayer};
use crate::{
    context::Endpoint,
    discovery::{Change, Discover},
    net::Address,
};
pub use adaptive::{AdaptiveBalance, AdaptiveConfig};
pub use consistent_hash::{ConsistentHashBalance, ConsistentHashOption};
pub use least_conn::LeastConnectionBalance;
pub use p2c::P2c;
pub use random::WeightedRandomBalance;
pub use response_time_weighted::ResponseTimeWeightedBalance;
pub use round_robin::{RoundRobinBalance, WeightedRoundRobinBalance};

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct RequestHash(pub u64);

/// [`LoadBalance`] promise the feature of the load balance policy.
pub trait LoadBalance<D>: Send + Sync + 'static
where
    D: Discover,
{
    /// `get_picker` allows to get an instance iterator of a specified endpoint from self or
    /// service discovery.
    fn get_picker<'future>(
        &'future self,
        endpoint: &'future Endpoint,
        discover: &'future D,
    ) -> futures::future::BoxFuture<
        'future,
        Result<Box<dyn Iterator<Item = Address> + Send>, LoadBalanceError>,
    >;

    /// `rebalance` is the callback method be used in service discovering subscription.
    fn rebalance(&self, changes: Change<D::Key>);
}

pub trait MkLbLayer {
    type Layer;

    fn make(self) -> Self::Layer;
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

impl<LB, DISC> MkLbLayer for LbConfig<LB, DISC> {
    type Layer = LoadBalanceLayer<DISC, LB>;

    fn make(self) -> Self::Layer {
        LoadBalanceLayer::new(self.discover, self.load_balance, self.retry_count)
    }
}

impl<L> MkLbLayer for CustomLayer<L> {
    type Layer = L;

    fn make(self) -> Self::Layer {
        self.0
    }
}

#[derive(Debug, Clone)]
pub enum LoadBalanceStrategy {
    RoundRobin,
    WeightedRoundRobin(Vec<(Address, usize)>),
    LeastConnection,
    WeightedLeastConnection(std::collections::HashMap<Address, usize>),
    ResponseTimeWeighted { window_size: usize },
    Adaptive(AdaptiveConfig),
    ConsistentHash(ConsistentHashOption),
    Random,
}

pub struct LoadBalanceFactory;

impl LoadBalanceFactory {
    pub fn create<D: Discover>(strategy: LoadBalanceStrategy) -> Box<dyn LoadBalance<D>> {
        match strategy {
            LoadBalanceStrategy::RoundRobin => Box::new(RoundRobinBalance::new()),
            LoadBalanceStrategy::WeightedRoundRobin(weights) => {
                Box::new(WeightedRoundRobinBalance::new(weights))
            }
            LoadBalanceStrategy::LeastConnection => Box::new(LeastConnectionBalance::new()),
            LoadBalanceStrategy::WeightedLeastConnection(weights) => {
                Box::new(LeastConnectionBalance::with_weights(weights))
            }
            LoadBalanceStrategy::ResponseTimeWeighted { window_size } => {
                Box::new(ResponseTimeWeightedBalance::new(window_size))
            }
            LoadBalanceStrategy::Adaptive(config) => Box::new(AdaptiveBalance::new(config)),
            LoadBalanceStrategy::ConsistentHash(options) => {
                Box::new(ConsistentHashBalance::new(options))
            }
            LoadBalanceStrategy::Random => Box::new(WeightedRandomBalance::new()),
        }
    }
}
