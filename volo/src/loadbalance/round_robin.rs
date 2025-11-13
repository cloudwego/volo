use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{LoadBalance, error::LoadBalanceError};
use crate::{
    context::Endpoint,
    discovery::{Change, Discover},
    net::Address,
};

pub struct RoundRobinBalance {
    counter: Arc<AtomicUsize>,
}

impl Default for RoundRobinBalance {
    fn default() -> Self {
          Self {
            counter: Arc::new(AtomicUsize::new(0)),
        }
       
    }
}

impl RoundRobinBalance {
    pub fn new() -> Self {
        Self::default()
    }
}

pub struct RoundRobinIterator {
    instances: Vec<Address>,
    current: usize,
    returned_count: usize,
}

impl RoundRobinIterator {
    pub fn new(instances: Vec<Address>, start_index: usize) -> Self {
        let current = if instances.is_empty() {
            0
        } else {
            start_index % instances.len()
        };
        Self {
            instances,
            current,
            returned_count: 0,
        }
    }
}

impl Iterator for RoundRobinIterator {
    type Item = Address;

    fn next(&mut self) -> Option<Self::Item> {
        if self.instances.is_empty() {
            return None;
        }

        let addr = self.instances[self.current].clone();
        self.current = (self.current + 1) % self.instances.len();
        self.returned_count += 1;
        Some(addr)
    }
}

impl<D> LoadBalance<D> for RoundRobinBalance
where
    D: Discover,
{
    fn get_picker<'future>(
        &'future self,
        endpoint: &'future Endpoint,
        discover: &'future D,
    ) -> futures::future::BoxFuture<
        'future,
        Result<Box<dyn Iterator<Item = Address> + Send>, LoadBalanceError>,
    > {
        Box::pin(async move {
            let instances = discover
                .discover(endpoint)
                .await
                .map_err(|_| LoadBalanceError::NoAvailableInstance)?;

            if instances.is_empty() {
                return Err(LoadBalanceError::NoAvailableInstance);
            }

            let addresses: Vec<Address> = instances
                .into_iter()
                .map(|instance| instance.address.clone())
                .collect();

            let current = self.counter.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(RoundRobinIterator::new(addresses, current))
                as Box<dyn Iterator<Item = Address> + Send>)
        })
    }

    /// Resets the round-robin counter to zero.
    /// This is typically called when the set of endpoints changes.
    fn rebalance(&self, _changes: Change<D::Key>) {
        self.counter.store(0, Ordering::SeqCst);
    }
}

#[derive(Clone)]
pub struct WeightedRoundRobinBalance {
    servers: Arc<parking_lot::RwLock<Vec<WeightedServer>>>,
}

#[derive(Debug, Clone)]
struct WeightedServer {
    /// The network address of the server
    address: Address,
    /// The static weight assigned to this server
    #[allow(dead_code)]
    weight: usize,
    /// The current dynamic weight used in the selection algorithm
    current_weight: isize,
    /// The effective weight considering server's health and performance
    effective_weight: isize,
}

impl WeightedRoundRobinBalance {
    pub fn new(servers: Vec<(Address, usize)>) -> Self {
        let servers = servers
            .into_iter()
            .map(|(addr, weight)| WeightedServer {
                address: addr,
                weight,
                current_weight: 0,
                effective_weight: weight as isize,
            })
            .collect();

        Self {
            servers: Arc::new(parking_lot::RwLock::new(servers)),
        }
    }

    pub fn update_servers(&self, servers: Vec<(Address, usize)>) {
        let new_servers = servers
            .into_iter()
            .map(|(addr, weight)| WeightedServer {
                address: addr,
                weight,
                current_weight: 0,
                effective_weight: weight as isize,
            })
            .collect();

        *self.servers.write() = new_servers;
    }

    fn select_server(&self) -> Option<Address> {
        let mut servers = self.servers.write();

        if servers.is_empty() {
            return None;
        }

        let mut total_weight = 0;
        let mut best_server = None;
        let mut best_weight = isize::MIN;

        for server in servers.iter_mut() {
            server.current_weight += server.effective_weight;
            total_weight += server.effective_weight;

            if server.current_weight > best_weight {
                best_weight = server.current_weight;
                best_server = Some(server.address.clone());
            }
        }

        if let Some(ref addr) = best_server {
            for server in servers.iter_mut() {
                if server.address == *addr {
                    server.current_weight -= total_weight;
                    break;
                }
            }
        }

        best_server
    }
}

pub struct WeightedRoundRobinIterator {
    balance: Arc<WeightedRoundRobinBalance>,
    rounds_left: usize,
}

impl Iterator for WeightedRoundRobinIterator {
    type Item = Address;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rounds_left == 0 {
            return None;
        }
        self.rounds_left -= 1;
        self.balance.select_server()
    }
}

impl<D> LoadBalance<D> for WeightedRoundRobinBalance
where
    D: Discover,
{
    fn get_picker<'future>(
        &'future self,
        endpoint: &'future Endpoint,
        discover: &'future D,
    ) -> futures::future::BoxFuture<
        'future,
        Result<Box<dyn Iterator<Item = Address> + Send>, LoadBalanceError>,
    > {
        let balance = self.clone();
        Box::pin(async move {
            let instances = discover
                .discover(endpoint)
                .await
                .map_err(|_| LoadBalanceError::NoAvailableInstance)?;

            if instances.is_empty() {
                return Err(LoadBalanceError::NoAvailableInstance);
            }

            let total_weight: usize = instances
                .iter()
                .map(|instance| instance.weight as usize)
                .sum();

            let servers_with_weight: Vec<(Address, usize)> = instances
                .into_iter()
                .map(|instance| (instance.address.clone(), instance.weight as usize))
                .collect();

            balance.update_servers(servers_with_weight);

            Ok(Box::new(WeightedRoundRobinIterator {
                balance: Arc::new(balance),
                rounds_left: total_weight,
            }) as Box<dyn Iterator<Item = Address> + Send>)
        })
    }

    fn rebalance(&self, _changes: Change<D::Key>) {
        // Reset server weights on rebalance
        let mut servers = self.servers.write();
        for server in servers.iter_mut() {
            server.current_weight = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::{ready, BoxFuture};
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use crate::discovery::Instance;

    #[derive(Default)]
    struct TestDiscover {
        instances: Vec<Arc<Instance>>,
    }

    #[allow(refining_impl_trait)]
    impl Discover for TestDiscover {
        type Key = String;
        type Error = Box<dyn std::error::Error + Send + Sync>;

        fn discover(
            &self,
            _endpoint: &Endpoint,
        ) -> BoxFuture<'static, Result<Vec<Arc<Instance>>, Self::Error>> {
            Box::pin(ready(Ok(self.instances.clone())))
        }

        fn key(&self, _endpoint: &Endpoint) -> Self::Key {
            "test".to_string()
        }

        fn watch(
            &self,
            _filter: Option<&[Self::Key]>,
        ) -> Option<async_broadcast::Receiver<Change<Self::Key>>> {
            None
        }
    }

    fn create_test_instance(port: u16, weight: u32) -> Arc<Instance> {
        let socket = SocketAddr::from(([127, 0, 0, 1], port));
        Arc::new(Instance {
            address: Address::from(socket),
            weight,
            tags: HashMap::new(),
        })
    }

    #[tokio::test]
    async fn test_round_robin_empty() {
        let lb = RoundRobinBalance::new();
        let discover = TestDiscover::default();
        let endpoint = Endpoint::new("test".into());

        let result = lb.get_picker(&endpoint, &discover).await;
        assert!(matches!(result, Err(LoadBalanceError::NoAvailableInstance)));
    }

    #[tokio::test]
    async fn test_round_robin_single() {
        let lb = RoundRobinBalance::new();
        let mut discover = TestDiscover::default();

        let instance = create_test_instance(8080, 100);
        discover.instances.push(instance.clone());

        let endpoint = Endpoint::new("test".into());
        let mut result = lb.get_picker(&endpoint, &discover).await.unwrap();

        // Single instance should always return that instance
        let picked_addr = result.next().unwrap();
        assert_eq!(picked_addr, instance.address);
    }

    #[tokio::test]
    async fn test_round_robin_multiple() {
        let lb = RoundRobinBalance::new();
        let mut discover = TestDiscover::default();

        let instance1 = create_test_instance(8080, 100);
        let instance2 = create_test_instance(8081, 100);
        let instance3 = create_test_instance(8082, 100);

        discover.instances.extend(vec![
            instance1.clone(),
            instance2.clone(),
            instance3.clone(),
        ]);

        let endpoint = Endpoint::new("test".into());
        let result = lb.get_picker(&endpoint, &discover).await.unwrap();

        // Test round robin order
        let picks: Vec<_> = result.take(6).collect();
        assert_eq!(picks.len(), 6, "Expected 6 addresses to be returned");

        // First round
        assert_eq!(picks[0], instance1.address);
        assert_eq!(picks[1], instance2.address);
        assert_eq!(picks[2], instance3.address);

        // Second round
        assert_eq!(picks[3], instance1.address);
        assert_eq!(picks[4], instance2.address);
        assert_eq!(picks[5], instance3.address);
    }

    #[tokio::test]
    async fn test_weighted_round_robin_empty() {
        let lb = WeightedRoundRobinBalance::new(vec![]);
        let discover = TestDiscover::default();
        let endpoint = Endpoint::new("test".into());

        let result = lb.get_picker(&endpoint, &discover).await;
        assert!(matches!(result, Err(LoadBalanceError::NoAvailableInstance)));
    }

    #[tokio::test]
    async fn test_weighted_round_robin_single() {
        let instance = create_test_instance(8080, 100);
        let lb = WeightedRoundRobinBalance::new(vec![(instance.address.clone(), instance.weight as usize)]);
        let mut discover = TestDiscover::default();

        discover.instances.push(instance.clone());

        let endpoint = Endpoint::new("test".into());
        let mut result = lb.get_picker(&endpoint, &discover).await.unwrap();

        // Single instance should always return that instance
        let picked_addr = result.next().unwrap();
        assert_eq!(picked_addr, instance.address);
    }

    #[tokio::test]
    async fn test_weighted_round_robin_multiple() {
        // Create instances with different weights
        let instance1 = create_test_instance(8080, 4); // Weight 4
        let instance2 = create_test_instance(8081, 2); // Weight 2
        let instance3 = create_test_instance(8082, 1); // Weight 1

        let lb = WeightedRoundRobinBalance::new(vec![
            (instance1.address.clone(), instance1.weight as usize),
            (instance2.address.clone(), instance2.weight as usize),
            (instance3.address.clone(), instance3.weight as usize),
        ]);
        let mut discover = TestDiscover::default();

        discover.instances.extend(vec![
            instance1.clone(),
            instance2.clone(),
            instance3.clone(),
        ]);

        let endpoint = Endpoint::new("test".into());
        let picker = lb.get_picker(&endpoint, &discover).await.unwrap();

        let picks: Vec<_> = picker.take(7).collect(); // One complete cycle (4 + 2 + 1 = 7 requests)

        let mut addr_counts = HashMap::new();
        for addr in picks {
            *addr_counts.entry(addr).or_insert(0) += 1;
        }

        // Verify distribution according to weights
        assert_eq!(*addr_counts.get(&instance1.address).unwrap_or(&0), 4); // Weight 4
        assert_eq!(*addr_counts.get(&instance2.address).unwrap_or(&0), 2); // Weight 2
        assert_eq!(*addr_counts.get(&instance3.address).unwrap_or(&0), 1); // Weight 1
    }

    #[tokio::test]
    async fn test_rebalance() {
        let lb = RoundRobinBalance::new();
        let mut discover = TestDiscover::default();

        let instance1 = create_test_instance(8080, 100);
        let instance2 = create_test_instance(8081, 100);
        let instances = vec![instance1.clone(), instance2.clone()];
        discover.instances.extend(instances.clone());

        let endpoint = Endpoint::new("test".into());

        // First round
        let mut result = lb.get_picker(&endpoint, &discover).await.unwrap();
        let first_pick = result.next().unwrap();

        // Rebalance with change notification
        let change = Change {
            key: "test".to_string(),
            all: instances.clone(),
            added: Vec::new(),
            updated: Vec::new(),
            removed: Vec::new(),
        };
        LoadBalance::<TestDiscover>::rebalance(&lb, change);

        // After rebalance, should start from beginning
        let mut result = lb.get_picker(&endpoint, &discover).await.unwrap();
        let second_pick = result.next().unwrap();

        assert_eq!(first_pick, second_pick);
    }

    #[tokio::test]
    async fn test_weighted_rebalance() {
        let instance1 = create_test_instance(8080, 2);
        let instance2 = create_test_instance(8081, 1);
        let lb = WeightedRoundRobinBalance::new(vec![
            (instance1.address.clone(), instance1.weight as usize),
            (instance2.address.clone(), instance2.weight as usize),
        ]);
        let mut discover = TestDiscover::default();

        let instances = vec![instance1.clone(), instance2.clone()];
        discover.instances.extend(instances.clone());

        let endpoint = Endpoint::new("test".into());

        // First picks
        let mut picks_before = Vec::new();
        for _ in 0..3 {
            let mut picker = lb.get_picker(&endpoint, &discover).await.unwrap();
            if let Some(addr) = picker.next() {
                picks_before.push(addr);
            }
        }

        // Rebalance with change notification
        let change = Change {
            key: "test".to_string(),
            all: instances.clone(),
            added: Vec::new(),
            updated: Vec::new(),
            removed: Vec::new(),
        };
        LoadBalance::<TestDiscover>::rebalance(&lb, change);

        // After rebalance
        let mut picks_after = Vec::new();
        for _ in 0..3 {
            let mut picker = lb.get_picker(&endpoint, &discover).await.unwrap();
            if let Some(addr) = picker.next() {
                picks_after.push(addr);
            }
        }

        assert_eq!(
            picks_before, picks_after,
            "Weight distribution should be consistent after rebalance"
        );
    }
}
