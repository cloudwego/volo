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

impl Discover for DnsResolver {
    type Key = FastStr;
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
        let (host, port) = parse_host_and_port(service_name)?;

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
        endpoint.service_name.clone()
    }

    fn watch(&self, _: Option<&[Self::Key]>) -> Option<Receiver<Change<Self::Key>>> {
        None
    }
}

fn parse_host_and_port(service_name: &str) -> Result<(&str, u16), LoadBalanceError> {
    // Parse IPv6 with optional port
    if let Some(rest) = service_name.strip_prefix('[') {
        if let Some(end_bracket) = rest.find(']') {
            let ip_part = &rest[..end_bracket];
            let port = if let Some(port_str) = rest[end_bracket + 1..].strip_prefix(':') {
                port_str
                    .parse::<u16>()
                    .map_err(|_| LoadBalanceError::Discover("invalid port number".into()))?
            } else {
                80
            };
            return Ok((ip_part, port));
        } else {
            return Err(LoadBalanceError::Discover("invalid IPv6 format".into()));
        }
    }

    // Parse IPv4 or domain with optional port
    let (host, port_str_opt) = if let Some((host, port_str)) = service_name.rsplit_once(':') {
        (host, Some(port_str))
    } else {
        (service_name, None)
    };

    let port = match port_str_opt {
        Some(port_str) => port_str
            .parse::<u16>()
            .map_err(|_| LoadBalanceError::Discover("invalid port number".into()))?,
        None => 80,
    };

    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_ipv6() {
        let resolver = DnsResolver::default();
        let endpoint = Endpoint {
            service_name: FastStr::from("[::1]"),
            ..Default::default()
        };
        let service_name = endpoint.service_name_ref();
        let result = parse_host_and_port(service_name).unwrap();
        assert_eq!(result, ("::1", 80));
        let result = resolver.discover(&endpoint).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_ipv6_with_port() {
        let resolver = DnsResolver::default();
        let endpoint = Endpoint {
            service_name: FastStr::from("[::1]:8080"),
            ..Default::default()
        };
        let service_name = endpoint.service_name_ref();
        let result = parse_host_and_port(service_name).unwrap();
        assert_eq!(result, ("::1", 8080));
        let result = resolver.discover(&endpoint).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_ipv4() {
        let resolver = DnsResolver::default();
        let endpoint = Endpoint {
            service_name: FastStr::from("127.0.0.1"),
            ..Default::default()
        };
        let service_name = endpoint.service_name_ref();
        let result = parse_host_and_port(service_name).unwrap();
        assert_eq!(result, ("127.0.0.1", 80));
        let result = resolver.discover(&endpoint).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_ipv4_with_port() {
        let resolver = DnsResolver::default();
        let endpoint = Endpoint {
            service_name: FastStr::from("127.0.0.1:8080"),
            ..Default::default()
        };
        let service_name = endpoint.service_name_ref();
        let result = parse_host_and_port(service_name).unwrap();
        assert_eq!(result, ("127.0.0.1", 8080));
        let result = resolver.discover(&endpoint).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_domain() {
        let resolver = DnsResolver::default();
        let endpoint = Endpoint {
            service_name: FastStr::from("example.com"),
            ..Default::default()
        };
        let service_name = endpoint.service_name_ref();
        let result = parse_host_and_port(service_name).unwrap();
        assert_eq!(result, ("example.com", 80));
        let result = resolver.discover(&endpoint).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_domain_with_port() {
        let resolver = DnsResolver::default();
        let endpoint = Endpoint {
            service_name: FastStr::from("example.com:8080"),
            ..Default::default()
        };
        let service_name = endpoint.service_name_ref();
        let result = parse_host_and_port(service_name).unwrap();
        assert_eq!(result, ("example.com", 8080));
        let result = resolver.discover(&endpoint).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_ipv6_invalid() {
        let resolver = DnsResolver::default();
        let endpoint = Endpoint {
            service_name: FastStr::from("[:1]"),
            ..Default::default()
        };
        let result = resolver.discover(&endpoint).await;
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "bad host name"
        ));
    }

    #[tokio::test]
    async fn test_ipv4_invalid() {
        let resolver = DnsResolver::default();
        let endpoint = Endpoint {
            service_name: FastStr::from("127.0.1:80"),
            ..Default::default()
        };
        let result = resolver.discover(&endpoint).await;
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "bad host name"
        ));
    }

    #[tokio::test]
    async fn test_domain_invalid() {
        let resolver = DnsResolver::default();
        let endpoint = Endpoint {
            service_name: FastStr::from("example"),
            ..Default::default()
        };
        let result = resolver.discover(&endpoint).await;
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "bad host name"
        ));
    }

    #[test]
    fn test_parse_invalid_ipv6() {
        let service_name = "[::1]:a";
        let result = parse_host_and_port(service_name);
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "invalid port number"
        ));
        let service_name = "[::1]:";
        let result = parse_host_and_port(service_name);
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "invalid port number"
        ));
        let service_name = "[::1]:70000";
        let result = parse_host_and_port(service_name);
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "invalid port number"
        ));
        let service_name = "[::1]:-1";
        let result = parse_host_and_port(service_name);
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "invalid port number"
        ));
        let service_name = "[::1";
        let result = parse_host_and_port(service_name);
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "invalid IPv6 format"
        ));
    }

    #[test]
    fn test_parse_invalid_ipv4() {
        let service_name = "127.0.0.1:a";
        let result = parse_host_and_port(service_name);
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "invalid port number"
        ));
        let service_name = "127.0.0.1:";
        let result = parse_host_and_port(service_name);
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "invalid port number"
        ));
        let service_name = "127.0.0.1:70000";
        let result = parse_host_and_port(service_name);
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "invalid port number"
        ));
        let service_name = "127.0.0.1:-1";
        let result = parse_host_and_port(service_name);
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "invalid port number"
        ));
    }

    #[test]
    fn test_parse_invalid_domain() {
        let service_name = "example.com:a";
        let result = parse_host_and_port(service_name);
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "invalid port number"
        ));
        let service_name = "example.com:";
        let result = parse_host_and_port(service_name);
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "invalid port number"
        ));
        let service_name = "example.com:70000";
        let result = parse_host_and_port(service_name);
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "invalid port number"
        ));
        let service_name = "example.com:-1";
        let result = parse_host_and_port(service_name);
        assert!(matches!(
            result,
            Err(LoadBalanceError::Discover(e)) if e.to_string() == "invalid port number"
        ));
    }
}
