use std::{
    net::{SocketAddr, SocketAddrV4, SocketAddrV6},
    sync::Arc,
};

use constant_dns::ConstantDnsDiscover;
use url::Host;
use volo::{
    context::Endpoint,
    discovery::{Discover, Instance, StaticDiscover},
    loadbalance::error::LoadBalanceError,
    net::Address,
};
use volo_http::client::dns::DnsResolver;

use super::endpoint::RpcEndpoint;

pub mod constant_dns;

struct LazyDiscoverInternal {
    endpoint: RpcEndpoint,
    resolver: DnsResolver,
}

#[derive(Clone)]
pub struct LazyDiscover {
    inner: Arc<LazyDiscoverInternal>,
}

impl LazyDiscover {
    pub fn new(endpoint: RpcEndpoint) -> Self {
        let resolver = DnsResolver::default();
        Self { inner: Arc::new(LazyDiscoverInternal { endpoint, resolver }) }
    }
}

impl Discover for LazyDiscover {
    type Key = ();
    type Error = LoadBalanceError;

    async fn discover<'s>(
        &'s self,
        endpoint: &'s Endpoint,
    ) -> Result<Vec<Arc<Instance>>, Self::Error> {
        let ep = self.inner.endpoint.clone();
        match ep.host {
            Host::Domain(domain) => {
                ConstantDnsDiscover::new(
                    self.inner.resolver.clone(),
                    domain.clone(),
                    domain,
                    ep.port,
                )
                .discover(endpoint)
                .await
            }
            Host::Ipv4(ip) => StaticDiscover::new(vec![Arc::new(Instance {
                address: Address::Ip(SocketAddr::V4(SocketAddrV4::new(
                    ip, ep.port,
                ))),
                weight: 1,
                tags: Default::default(),
            })])
            .discover(endpoint)
            .await
            .map_err(|_e| LoadBalanceError::Retry),
            Host::Ipv6(ip) => StaticDiscover::new(vec![Arc::new(Instance {
                address: Address::Ip(SocketAddr::V6(SocketAddrV6::new(
                    ip, ep.port, 0, 0,
                ))),
                weight: 1,
                tags: Default::default(),
            })])
            .discover(endpoint)
            .await
            .map_err(|_e| LoadBalanceError::Retry),
        }
    }

    fn key(&self, _endpoint: &volo::context::Endpoint) -> Self::Key {}

    fn watch(
        &self,
        _keys: Option<&[Self::Key]>,
    ) -> Option<async_broadcast::Receiver<volo::discovery::Change<Self::Key>>>
    {
        None
    }
}
