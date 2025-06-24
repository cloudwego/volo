//! DNS resolver implementation
//!
//! This module implements [`DnsResolver`] as a [`Discover`] for client.

use std::{net::SocketAddr, sync::Arc};

use async_broadcast::Receiver;
use faststr::FastStr;
use hickory_resolver::{
    config::{LookupIpStrategy, ResolverConfig, ResolverOpts},
    name_server::TokioConnectionProvider,
    Resolver, TokioResolver,
};
use http::uri::Port;
use volo::{
    context::Endpoint,
    discovery::{Change, Discover, Instance},
    loadbalance::error::LoadBalanceError,
    net::Address,
};

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
            .expect("[Volo-gRPC] DnsResolver: failed to parse dns config");
        if conf
            .name_servers()
            .first()
            .expect("[Volo-gRPC] DnsResolver: no nameserver found")
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
        let port = ep
            .get::<Port<u16>>()
            .map(|p| p.as_u16())
            .unwrap_or_default();
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
        let port = match endpoint.get::<Port<u16>>() {
            Some(port) => port.as_u16(),
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
