use futures::future::BoxFuture;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use super::{LoadBalance, error::LoadBalanceError};
use crate::{
    context::Endpoint,
    discovery::{Change, Discover},
    net::Address,
};

pub struct LeastConnectionBalance {
    connections: Arc<RwLock<HashMap<Address, ConnectionInfo>>>,
}

#[derive(Debug, Clone)]
struct ConnectionInfo {
    active_count: usize,
    total_count: usize,
    weight: usize,
}

impl Default for LeastConnectionBalance {
    fn default() -> Self {
        Self{
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl LeastConnectionBalance {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_weights(weights: HashMap<Address, usize>) -> Self {
        let connections = weights
            .into_iter()
            .map(|(addr, weight)| {
                (
                    addr,
                    ConnectionInfo {
                        active_count: 0,
                        total_count: 0,
                        weight,
                    },
                )
            })
            .collect();

        Self {
            connections: Arc::new(RwLock::new(connections)),
        }
    }

    fn select_least_conn_server(&self) -> Option<Address> {
        let connections = self.connections.read();

        connections
            .iter()
            .min_by_key(|(_, info)| {
                if info.weight > 0 {
                    info.active_count * 100 / info.weight
                } else {
                    info.active_count
                }
            })
            .map(|(addr, _)| addr.clone())
    }

    pub fn on_connection_start(&self, addr: &Address) {
        let mut connections = self.connections.write();
        if let Some(info) = connections.get_mut(addr) {
            info.active_count += 1;
            info.total_count += 1;
        }
    }

    pub fn on_connection_end(&self, addr: &Address) {
        let mut connections = self.connections.write();
        if let Some(info) = connections.get_mut(addr) {
            info.active_count = info.active_count.saturating_sub(1);
        }
    }

    fn update_servers(&self, addresses: Vec<Address>) {
        let mut connections = self.connections.write();

        connections.retain(|addr, _| addresses.contains(addr));

        for addr in addresses {
            connections.entry(addr).or_insert(ConnectionInfo {
                active_count: 0,
                total_count: 0,
                weight: 1,
            });
        }
    }
}

pub struct LeastConnectionIterator {
    balance: Arc<LeastConnectionBalance>,
    returned: bool,
}

impl Iterator for LeastConnectionIterator {
    type Item = Address;

    fn next(&mut self) -> Option<Self::Item> {
        if self.returned {
            return None;
        }
        self.returned = true;

        let addr = self.balance.select_least_conn_server();
        if let Some(ref address) = addr {
            self.balance.on_connection_start(address);
        }
        addr
    }
}

impl<D> LoadBalance<D> for LeastConnectionBalance
where
    D: Discover,
{
    fn get_picker<'future>(
        &'future self,
        endpoint: &'future Endpoint,
        discover: &'future D,
    ) -> BoxFuture<'future, Result<Box<dyn Iterator<Item = Address> + Send>, LoadBalanceError>>
    {
        Box::pin(async move {
            let instances = discover
                .discover(endpoint)
                .await
                .map_err(|_| LoadBalanceError::NoAvailableInstance)?;

            let addresses: Vec<Address> = instances
                .into_iter()
                .map(|instance| instance.address.clone())
                .collect();

            self.update_servers(addresses);

            Ok(Box::new(LeastConnectionIterator {
                balance: Arc::new(self.clone()),
                returned: false,
            }) as Box<dyn Iterator<Item = Address> + Send>)
        })
    }

    fn rebalance(&self, _changes: Change<D::Key>) {}
}

impl Clone for LeastConnectionBalance {
    fn clone(&self) -> Self {
        Self {
            connections: Arc::clone(&self.connections),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::StaticDiscover;

    #[tokio::test]
    async fn test_least_connection_balance() {
        let addresses = vec![
            "127.0.0.1:8080".parse().unwrap(),
            "127.0.0.1:8081".parse().unwrap(),
        ];

        let discover = StaticDiscover::from(addresses.clone());
        let lb = LeastConnectionBalance::new();

        let endpoint = Endpoint::new("test".into());

        let mut picker1 = lb.get_picker(&endpoint, &discover).await.unwrap();
        let selected1 = picker1.next().unwrap();

        let mut picker2 = lb.get_picker(&endpoint, &discover).await.unwrap();
        let selected2 = picker2.next().unwrap();

        assert_ne!(selected1, selected2);

        lb.on_connection_end(&selected1);

        let mut picker3 = lb.get_picker(&endpoint, &discover).await.unwrap();
        let selected3 = picker3.next().unwrap();

        assert_eq!(selected1, selected3);
    }
}
