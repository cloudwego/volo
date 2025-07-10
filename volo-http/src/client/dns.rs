//! DNS resolver implementation
//!
//! This module implements [`DnsResolver`] as a [`Discover`] for client.

use std::{
    net::{IpAddr, SocketAddr},
    ops::Deref,
    sync::Arc,
};

use async_broadcast::Receiver;
use faststr::FastStr;
use hickory_resolver::{
    Resolver, TokioResolver,
    config::{LookupIpStrategy, ResolverConfig, ResolverOpts},
    name_server::TokioConnectionProvider,
};
use volo::{
    context::Endpoint,
    discovery::{Change, Discover, Instance},
    loadbalance::error::LoadBalanceError,
    net::Address,
};

use crate::error::client::{bad_host_name, no_address};

/// The port for `DnsResolver`, and only used for `DnsResolver`.
///
/// When resolving domain name, the response is only an IP address without port, but to access the
/// destination server, the port is needed.
///
/// For setting port to `DnsResolver`, you can insert it into `Endpoint` of `callee` in
/// `ClientContext`, the resolver will apply it.
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

    /// Resolve a host to an IP address.
    pub async fn resolve(&self, host: &str) -> Option<IpAddr> {
        // Note that the Resolver will try to parse the host as an IP address first, so we don't
        // need to parse it manually.
        self.resolver.lookup_ip(host).await.ok()?.into_iter().next()
    }
}

impl Default for DnsResolver {
    fn default() -> Self {
        let (conf, mut opts) = hickory_resolver::system_conf::read_system_conf()
            .expect("[Volo-HTTP] DnsResolver: failed to parse dns config");
        if conf
            .name_servers()
            .first()
            .expect("[Volo-HTTP] DnsResolver: no nameserver found")
            .socket_addr
            .is_ipv6()
        {
            // The default `LookupIpStrategy` is always `Ipv4thenIpv6`, it may not work in an IPv6
            // only environment.
            //
            // Here we trust the system configuration and check its first name server.
            //
            // If the first nameserver is an IPv4 address, we keep the default configuration.
            //
            // If the first nameserver is an IPv6 address, we need to update the policy to prefer
            // IPv6 addresses.
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
        if endpoint.address().is_some() {
            return Ok(Vec::new());
        }
        if endpoint.service_name_ref().is_empty() {
            tracing::error!("[Volo-HTTP] DnsResolver: no domain name found");
            return Err(LoadBalanceError::Discover(Box::new(no_address())));
        }
        let port = match endpoint.get::<Port>() {
            Some(port) => port.0,
            None => {
                unreachable!();
            }
        };

        if let Some(ip) = self.resolve(endpoint.service_name_ref()).await {
            let address = Address::Ip(SocketAddr::new(ip, port));
            let instance = Instance {
                address,
                weight: 10,
                tags: Default::default(),
            };
            return Ok(vec![Arc::new(instance)]);
        };
        tracing::error!("[Volo-HTTP] DnsResolver: no address resolved");
        Err(LoadBalanceError::Discover(Box::new(bad_host_name(
            endpoint.service_name(),
        ))))
    }

    fn key(&self, endpoint: &Endpoint) -> Self::Key {
        DiscoverKey::from_endpoint(endpoint)
    }

    fn watch(&self, _: Option<&[Self::Key]>) -> Option<Receiver<Change<Self::Key>>> {
        None
    }
}

#[cfg(test)]
mod dns_tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use crate::client::dns::DnsResolver;

    #[tokio::test]
    async fn static_resolve() {
        let resolver = DnsResolver::default();

        assert_eq!(
            resolver.resolve("127.0.0.1").await,
            Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))),
        );
        assert_eq!(
            resolver.resolve("::1").await,
            Some(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1))),
        );
        assert_eq!(resolver.resolve("[::1]").await, None);
    }
}
