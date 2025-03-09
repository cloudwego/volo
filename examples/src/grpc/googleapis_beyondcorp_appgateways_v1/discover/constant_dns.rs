use std::sync::Arc;

use anyhow::anyhow;
use async_broadcast::Receiver;
use volo::{
    context::Endpoint,
    discovery::{Discover, Instance},
    loadbalance::error::LoadBalanceError,
};
use volo_http::client::dns::DnsResolver;

#[derive(Clone)]
pub struct ConstantDnsDiscover {
    resolver: DnsResolver,
    service_name: String,
    host: String,
    port: u16,
}

impl ConstantDnsDiscover {
    pub fn new(resolver: DnsResolver, service_name: String, host: String, port: u16) -> Self {
        Self {
            resolver,
            service_name,
            host,
            port,
        }
    }
}

impl Discover for ConstantDnsDiscover {
    type Key = ();
    type Error = LoadBalanceError;

    async fn discover<'s>(
        &'s self,
        _endpoint: &'s Endpoint,
    ) -> Result<Vec<Arc<Instance>>, Self::Error> {
        let mut endpoint = Endpoint::new(self.service_name.clone().into());
        let addr = self
            .resolver
            .resolve(&self.host, self.port)
            .await
            .ok_or_else(|| {
                LoadBalanceError::Discover(anyhow!("unable to resolve: {}", &self.host).into())
            })?;
        endpoint.set_address(addr);
        self.resolver.discover(&endpoint).await
    }

    fn key(&self, _endpoint: &Endpoint) -> Self::Key {}

    fn watch(
        &self,
        _keys: Option<&[Self::Key]>,
    ) -> Option<Receiver<volo::discovery::Change<Self::Key>>> {
        None
    }
}
