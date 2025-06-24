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
    ops::Deref,
    sync::Arc,
};

use async_broadcast::Receiver;
use faststr::FastStr;
use hickory_resolver::{
    config::{LookupIpStrategy, ResolverConfig, ResolverOpts},
    name_server::TokioConnectionProvider,
    Resolver, TokioResolver,
};

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

    /// `discover` allows to request an endpoint and return a discover future.
    fn discover<'s>(
        &'s self,
        endpoint: &'s Endpoint,
    ) -> impl Future<Output = Result<Vec<Arc<Instance>>, Self::Error>> + Send;
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

/// Get the difference of two address lists.
///
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

    async fn discover<'s>(&'s self, _: &'s Endpoint) -> Result<Vec<Arc<Instance>>, Self::Error> {
        Ok(self.instances.clone())
    }

    fn key(&self, _: &Endpoint) -> Self::Key {}

    fn watch(&self, _keys: Option<&[Self::Key]>) -> Option<Receiver<Change<Self::Key>>> {
        None
    }
}

/// [`WeightedStaticDiscover`] is a simple implementation of [`Discover`] that returns a static list
/// of instances with weight.
#[derive(Clone)]
pub struct WeightedStaticDiscover {
    instances: Vec<Arc<Instance>>,
}

impl WeightedStaticDiscover {
    /// Creates a new [`StaticDiscover`].
    pub fn new(instances: Vec<Arc<Instance>>) -> Self {
        Self { instances }
    }
}

impl From<Vec<(SocketAddr, u32)>> for WeightedStaticDiscover {
    fn from(addrs: Vec<(SocketAddr, u32)>) -> Self {
        let instances = addrs
            .into_iter()
            .map(|addr| {
                Arc::new(Instance {
                    address: Address::Ip(addr.0),
                    weight: addr.1,
                    tags: Default::default(),
                })
            })
            .collect();
        Self { instances }
    }
}

impl Discover for WeightedStaticDiscover {
    type Key = ();
    type Error = Infallible;

    async fn discover<'s>(&'s self, _: &'s Endpoint) -> Result<Vec<Arc<Instance>>, Self::Error> {
        Ok(self.instances.clone())
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

    async fn discover<'s>(&'s self, _: &'s Endpoint) -> Result<Vec<Arc<Instance>>, Self::Error> {
        Ok(vec![])
    }

    fn key(&self, _: &Endpoint) {}

    fn watch(&self, _keys: Option<&[Self::Key]>) -> Option<Receiver<Change<Self::Key>>> {
        None
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Port(pub u16);

impl Deref for Port {
    type Target = u16;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A service discover implementation for DNS.
#[derive(Clone)]
pub struct DnsResolver {
    resolver: TokioResolver,
}

impl DnsResolver {
    /// Build a new `DnsResolver` through `ResolverConfig` and `ResolverOpts`.
    ///
    /// For using system config, you can create a new instance by `DnsResolver::default()`.
    pub fn new(config: ResolverConfig, options: ResolverOpts) -> Self {
        let mut builder = Resolver::builder_with_config(config, TokioConnectionProvider::default());
        builder.options_mut().clone_from(&options);
        let resolver = builder.build();
        Self { resolver }
    }

    /// Resolve a host to an IP address and then set the port to it for getting an [`Address`].
    pub async fn resolve(&self, host: &str, port: u16) -> Option<Address> {
        let mut iter = self.resolver.lookup_ip(host).await.ok()?.into_iter();
        Some(Address::Ip(SocketAddr::new(iter.next()?, port)))
    }
}

impl Default for DnsResolver {
    fn default() -> Self {
        let (conf, mut opts) = hickory_resolver::system_conf::read_system_conf()
            .expect("DnsResolver: failed to parse dns config");
        if conf
            .name_servers()
            .first()
            .expect("DnsResolver: no nameserver found")
            .socket_addr
            .is_ipv6()
        {
            opts.ip_strategy = LookupIpStrategy::Ipv6thenIpv4;
        }
        Self::new(conf, opts)
    }
}

/// `Key` used to cache for [`Discover`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DiscoverKey {
    /// Service name for Service Discover, it's domain name for DNS by default.
    pub name: FastStr,
    /// Port for the service name, it's unnecessary for Service Discover, but it's important to
    /// cache as key.
    pub port: u16,
}

impl DiscoverKey {
    /// Get [`DiscoverKey`] from an [`Endpoint`].
    pub fn from_endpoint(ep: &Endpoint) -> Self {
        let name = ep.service_name();
        let port = ep.get::<Port>().cloned().unwrap_or_default().0;
        Self { name, port }
    }
}

impl Discover for DnsResolver {
    type Key = DiscoverKey;
    type Error = LoadBalanceError;

    async fn discover<'s>(
        &'s self,
        endpoint: &'s Endpoint,
    ) -> Result<Vec<Arc<Instance>>, Self::Error> {
        if endpoint.service_name_ref().is_empty() && endpoint.address().is_none() {
            tracing::error!("DnsResolver: no domain name found");
            return Err(LoadBalanceError::Discover("missing target address".into()));
        }
        if let Some(address) = endpoint.address() {
            let instance = Instance {
                address,
                weight: 10,
                tags: Default::default(),
            };
            return Ok(vec![Arc::new(instance)]);
        }

        let service_name = endpoint.service_name_ref();
        // Parse service name to get host name and port number (if any)
        let (host, port_str) = if let Some((host, port_str)) = service_name.rsplit_once(':') {
            (host, Some(port_str))
        } else {
            (service_name, None)
        };

        // Default to port 80 if port number does not exist
        let port = match endpoint.get::<Port>() {
            Some(port) => port.0,
            None => match port_str {
                Some(port_str) => port_str
                    .parse::<u16>()
                    .map_err(|_| LoadBalanceError::Discover("invalid port number".into()))?,
                None => 80,
            },
        };

        if let Some(address) = self.resolve(host, port).await {
            let instance = Instance {
                address,
                weight: 10,
                tags: Default::default(),
            };
            return Ok(vec![Arc::new(instance)]);
        };
        tracing::error!("DnsResolver: no address resolved");
        Err(LoadBalanceError::Discover("bad host name".into()))
    }

    fn key(&self, endpoint: &Endpoint) -> Self::Key {
        DiscoverKey::from_endpoint(endpoint)
    }

    fn watch(&self, _: Option<&[Self::Key]>) -> Option<Receiver<Change<Self::Key>>> {
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

    use super::{Discover, Instance, StaticDiscover, WeightedStaticDiscover};
    use crate::{context::Endpoint, net::Address};

    #[test]
    fn test_static_discover() {
        let empty = Endpoint::new("".into());
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

    #[test]
    fn test_weighted_static_discover() {
        let empty = Endpoint::new("".into());
        let discover = WeightedStaticDiscover::from(vec![
            ("127.0.0.1:8000".parse().unwrap(), 2),
            ("127.0.0.2:9000".parse().unwrap(), 3),
            ("127.0.0.3:9000".parse().unwrap(), 4),
        ]);
        let resp = futures::executor::block_on(async { discover.discover(&empty).await }).unwrap();
        let expected = vec![
            Arc::new(Instance {
                address: Address::Ip("127.0.0.1:8000".parse().unwrap()),
                weight: 2,
                tags: Default::default(),
            }),
            Arc::new(Instance {
                address: Address::Ip("127.0.0.2:9000".parse().unwrap()),
                weight: 3,
                tags: Default::default(),
            }),
            Arc::new(Instance {
                address: Address::Ip("127.0.0.3:9000".parse().unwrap()),
                weight: 4,
                tags: Default::default(),
            }),
        ];
        assert_eq!(resp, expected);
    }
}
