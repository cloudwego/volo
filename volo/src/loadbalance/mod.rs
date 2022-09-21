pub mod error;
mod layer;
pub mod random;

use std::future::Future;

use self::{error::LoadBalanceError, layer::LoadBalanceLayer};
use crate::{
    context::Endpoint,
    discovery::{Change, Discover},
    net::Address,
};

/// [`LoadBalance`] promise the feature of the load balance policy.
pub trait LoadBalance<D>: Send + Sync + 'static
where
    D: Discover,
{
    /// `InstanceIter` is an iterator of [`crate::discovery::Instance`].
    type InstanceIter<'iter>: Iterator<Item = Address> + Send + 'iter;

    /// `GetFut` is the return type of `get_picker`.
    type GetFut<'future, 'iter>: Future<Output = Result<Self::InstanceIter<'iter>, LoadBalanceError>>
        + Send; // remove +'future temporarily, see https://github.com/rust-lang/rust/issues/100013

    /// `get_picker` allows to get an instance iterator of a specified endpoint from self or
    /// service discovery.
    fn get_picker<'future, 'iter>(
        &'iter self,
        endpoint: &'future Endpoint,
        discover: &'future D,
    ) -> Self::GetFut<'future, 'iter>;
    /// `rebalance` is the callback method be used in service discovering subscription.
    fn rebalance(&self, changes: Change<D::Key>);
}

pub trait MkLbLayer<S> {
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

impl<LB, DISC, S> MkLbLayer<S> for LbConfig<LB, DISC> {
    type Layer = LoadBalanceLayer<DISC, LB>;

    fn make(self) -> Self::Layer {
        LoadBalanceLayer::new(self.discover, self.load_balance, self.retry_count)
    }
}

impl<S, L> MkLbLayer<S> for CustomLayer<L> {
    type Layer = L;

    fn make(self) -> Self::Layer {
        self.0
    }
}
