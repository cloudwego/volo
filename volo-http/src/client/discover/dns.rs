use std::{net::SocketAddr, ops::Deref, sync::Arc};

use async_broadcast::Receiver;
use faststr::FastStr;
use hickory_resolver::{
    config::{ResolverConfig, ResolverOpts},
    AsyncResolver, TokioAsyncResolver,
};
use volo::{
    context::Endpoint,
    discovery::{Change, Discover, Instance},
    loadbalance::error::LoadBalanceError,
    net::Address,
};

use super::{Target, TargetInner};
#[cfg(feature = "__tls")]
use crate::{client::transport::TlsTransport, utils::consts::HTTPS_DEFAULT_PORT};
use crate::{
    error::client::{bad_host_name, no_address},
    utils::consts::HTTP_DEFAULT_PORT,
};

/// The port for `DnsResolver`, and only used for `DnsResolver`.
///
/// When resolving domain name, the response is only an IP address without port, but to access the
/// destination server, the port is needed.
///
/// For setting port to `DnsResolver`, you can insert it into `Endpoint` of `callee` in
/// `ClientContext`, the resolver will apply it.
pub struct Port(pub u16);

impl Deref for Port {
    type Target = u16;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A service discover implementation for DNS.
///
/// This type
#[derive(Clone)]
pub struct DnsResolver {
    resolver: TokioAsyncResolver,
}

impl DnsResolver {
    /// Build a new `DnsResolver` through `ResolverConfig` and `ResolverOpts`.
    ///
    /// For using system config, you can create a new instance by `DnsResolver::default()`.
    pub fn new(config: ResolverConfig, options: ResolverOpts) -> Self {
        Self {
            resolver: AsyncResolver::tokio(config, options),
        }
    }

    pub async fn resolve(&self, host: &str, port: u16) -> Option<Address> {
        // Note that the Resolver will try to parse the host as an IP address first, so we don't
        // need to parse it manually.
        let mut iter = self.resolver.lookup_ip(host).await.ok()?.into_iter();
        Some(Address::Ip(SocketAddr::new(iter.next()?, port)))
    }
}

impl Default for DnsResolver {
    fn default() -> Self {
        Self {
            resolver: AsyncResolver::tokio_from_system_conf().expect("failed to init dns resolver"),
        }
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
            tracing::error!("[Volo-HTTP] DnsResolver: no domain name found");
            return Err(LoadBalanceError::Discover(Box::new(no_address())));
        }
        if let Some(address) = endpoint.address() {
            let instance = Instance {
                address,
                weight: 10,
                tags: Default::default(),
            };
            return Ok(vec![Arc::new(instance)]);
        }
        let port = match endpoint.get::<Port>() {
            Some(port) => port.0,
            None => {
                #[cfg(feature = "__tls")]
                if endpoint.contains::<TlsTransport>() {
                    HTTPS_DEFAULT_PORT
                } else {
                    HTTP_DEFAULT_PORT
                }
                #[cfg(not(feature = "__tls"))]
                HTTP_DEFAULT_PORT
            }
        };

        if let Some(address) = self.resolve(endpoint.service_name_ref(), port).await {
            let instance = Instance {
                address,
                weight: 10,
                tags: Default::default(),
            };
            return Ok(vec![Arc::new(instance)]);
        };
        tracing::error!("[Volo-HTTP] DnsResolver: no address resolved");
        Err(LoadBalanceError::Discover(Box::new(bad_host_name())))
    }

    fn key(&self, endpoint: &Endpoint) -> Self::Key {
        endpoint.service_name()
    }

    fn watch(&self, _: Option<&[Self::Key]>) -> Option<Receiver<Change<Self::Key>>> {
        None
    }
}

pub fn parse_target(target: &Target, endpoint: &mut Endpoint) {
    match target.inner() {
        TargetInner::None => {
            // `cargo-clippy` will warn on the `return` when `__tls` is disabled, just ignore it.
            #[allow(clippy::needless_return)]
            return;
        }
        TargetInner::Address(addr) => {
            endpoint.set_address(addr.clone());
        }
        TargetInner::Host { host, port } => {
            endpoint.insert(Port(*port));
            endpoint.set_service_name(host.clone());
        }
    }

    #[cfg(feature = "__tls")]
    if target.is_https() {
        endpoint.insert(TlsTransport);
    }
}
