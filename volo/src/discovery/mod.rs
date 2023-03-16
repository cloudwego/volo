//! This module cantains the default abstraction for service discovery of volo.
//!
//! We encourage users to use these traits to implement their own service discovery and
//! loadbalancer, so that we are able to reuse the same service discovery and loadbalancer
//! implementation.
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    convert::Infallible,
    future::Future,
    hash::Hash,
    net::SocketAddr,
    sync::Arc,
};

use async_broadcast::Receiver;

use crate::{context::Endpoint, loadbalance::error::LoadBalanceError, net::Address};

/// [`Instance`] contains information of an instance from the target service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub address: Address,
    pub weight: u32,
    pub tags: HashMap<Cow<'static, str>, Cow<'static, str>>,
}

/// [`Discover`] is the most basic trait for Discover.
pub trait Discover: Send + Sync + 'static {
    /// `Key` identifies a group of instances, such as the cluster name.
    type Key: Hash + PartialEq + Eq + Send + Sync + Clone + 'static;
    /// `Error` is the discovery error.
    type Error: Into<LoadBalanceError>;
    /// `DiscFut` is a Future object which returns a discovery result.
    type DiscFut<'future>: Future<Output = Result<Vec<Arc<Instance>>, Self::Error>> + Send + 'future;

    /// `discover` allows to request an endpoint and return a discover future.
    fn discover<'s>(&'s self, endpoint: &'s Endpoint) -> Self::DiscFut<'s>;
    /// `key` should return a key suitable for cache.
    fn key(&self, endpoint: &Endpoint) -> Self::Key;
    /// `watch` should return a [`async_broadcast::Receiver`] which can be used to subscribe
    /// [`Change`].
    fn watch(&self, keys: Option<&[Self::Key]>) -> Option<Receiver<Change<Self::Key>>>;
}

/// Change indicates the change of the service discover.
///
/// Change contains the difference between the current discovery result and the previous one.
/// It is designed for providing detail information when dispatching an event for service
/// discovery result change.
///
/// Since the loadbalancer may rely on caching the result of discover to improve performance,
/// the discover implementation should dispatch an event when result changes.
#[derive(Debug, Clone)]
pub struct Change<K> {
    /// `key` should be the same as the output of `WatchableDiscover::key`,
    /// which is often used by cache.
    pub key: K,
    pub all: Vec<Arc<Instance>>,
    pub added: Vec<Arc<Instance>>,
    pub updated: Vec<Arc<Instance>>,
    pub removed: Vec<Arc<Instance>>,
}

/// [`diff_address`] provides a naive implementation that compares prev and next only by the
/// address, and returns the [`Change`], which means that the `updated` is always empty when using
/// this implementation.
///
/// The bool in the return value indicates whether there's diff between prev and next, which means
/// that if the bool is false, the [`Change`] should be ignored, and the discover should not send
/// the event to loadbalancer.
///
/// If users need to compare the instances by also weight or tags, they should not use this.
pub fn diff_address<K>(
    key: K,
    prev: Vec<Arc<Instance>>,
    next: Vec<Arc<Instance>>,
) -> (Change<K>, bool)
where
    K: Hash + PartialEq + Eq + Send + Sync + 'static,
{
    let mut added = Vec::new();
    let updated = Vec::new();
    let mut removed = Vec::new();

    let mut prev_set = HashSet::with_capacity(prev.len());
    let mut next_set = HashSet::with_capacity(next.len());
    for i in &prev {
        prev_set.insert(i.address.clone());
    }
    for i in &next {
        next_set.insert(i.address.clone());
    }

    for i in &next {
        if !prev_set.contains(&i.address) {
            added.push(i.clone());
        }
    }
    for i in &prev {
        if !next_set.contains(&i.address) {
            removed.push(i.clone());
        }
    }

    let changed = !added.is_empty() || !removed.is_empty();

    (
        Change {
            key,
            all: next,
            added,
            updated,
            removed,
        },
        changed,
    )
}

/// [`StaticDiscover`] is a simple implementation of [`Discover`] that returns a static list of
/// instances.
#[derive(Clone)]
pub struct StaticDiscover {
    instances: Vec<Arc<Instance>>,
}

impl StaticDiscover {
    /// Creates a new [`StaticDiscover`].
    pub fn new(instances: Vec<Arc<Instance>>) -> Self {
        Self { instances }
    }
}

impl From<Vec<SocketAddr>> for StaticDiscover {
    fn from(addrs: Vec<SocketAddr>) -> Self {
        let instances = addrs
            .into_iter()
            .map(|addr| {
                Arc::new(Instance {
                    address: Address::Ip(addr),
                    weight: 1,
                    tags: Default::default(),
                })
            })
            .collect();
        Self { instances }
    }
}

impl Discover for StaticDiscover {
    type Key = ();
    type Error = Infallible;
    type DiscFut<'a> = impl Future<Output = Result<Vec<Arc<Instance>>, Self::Error>> + 'a;

    fn discover(&self, _: &Endpoint) -> Self::DiscFut<'_> {
        async { Ok(self.instances.clone()) }
    }

    fn key(&self, _: &Endpoint) -> Self::Key {}

    fn watch(&self, _keys: Option<&[Self::Key]>) -> Option<Receiver<Change<Self::Key>>> {
        None
    }
}

/// [`DummyDiscover`] always returns an empty list.
///
/// Users that don't specify the address directly need to use their own [`Discover`].
#[derive(Clone)]
pub struct DummyDiscover;

impl Discover for DummyDiscover {
    type Key = ();
    type Error = Infallible;
    type DiscFut<'a> = impl Future<Output = Result<Vec<Arc<Instance>>, Self::Error>> + 'a;

    fn discover(&self, _: &Endpoint) -> Self::DiscFut<'_> {
        async { Ok(vec![]) }
    }

    fn key(&self, _: &Endpoint) {}

    fn watch(&self, _keys: Option<&[Self::Key]>) -> Option<Receiver<Change<Self::Key>>> {
        None
    }
}

impl From<Infallible> for LoadBalanceError {
    fn from(_: Infallible) -> Self {
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{Discover, Instance, StaticDiscover};
    use crate::{context::Endpoint, net::Address};

    #[test]
    fn test_static_discover() {
        let empty = Endpoint {
            service_name: faststr::FastStr::new(""),
            address: None,
            tags: Default::default(),
        };
        let discover = StaticDiscover::from(vec![
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.2:9000".parse().unwrap(),
        ]);
        let resp = futures::executor::block_on(async { discover.discover(&empty).await }).unwrap();
        let expected = vec![
            Arc::new(Instance {
                address: Address::Ip("127.0.0.1:8000".parse().unwrap()),
                weight: 1,
                tags: Default::default(),
            }),
            Arc::new(Instance {
                address: Address::Ip("127.0.0.2:9000".parse().unwrap()),
                weight: 1,
                tags: Default::default(),
            }),
        ];
        assert_eq!(resp, expected);
    }
}
