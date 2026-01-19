use parking_lot::RwLock;
use rand::Rng;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{LoadBalance, error::LoadBalanceError};
use crate::{
    context::Endpoint,
    discovery::{Change, Discover},
    net::Address,
};
use futures::future::BoxFuture;

/// Load balancer that weighs servers based on their response times
pub struct ResponseTimeWeightedBalance {
    /// Map of server addresses to their performance metrics
    servers: Arc<RwLock<HashMap<Address, ServerMetrics>>>,
    /// Size of the sliding window for response time calculations
    window_size: usize,
}

#[derive(Debug)]
struct ServerMetrics {
    /// Queue of historical response times within the window
    response_times: VecDeque<Duration>,
    /// Timestamp of the last metrics update
    last_update: Instant,
    /// Dynamic weight calculated from response times
    weight: f64,
    /// Total number of requests processed by this server
    request_count: u64,
}

impl ResponseTimeWeightedBalance {
    pub fn new(window_size: usize) -> Self {
        Self {
            servers: Arc::new(RwLock::new(HashMap::new())),
            window_size,
        }
    }

    pub fn record_response_time(&self, addr: &Address, duration: Duration) {
        let mut servers = self.servers.write();
        let metrics = servers
            .entry(addr.clone())
            .or_insert_with(|| ServerMetrics {
                response_times: VecDeque::new(),
                last_update: Instant::now(),
                weight: 1.0,
                request_count: 0,
            });

        metrics.response_times.push_back(duration);
        if metrics.response_times.len() > self.window_size {
            metrics.response_times.pop_front();
        }

        metrics.request_count += 1;

        if !metrics.response_times.is_empty() {
            let avg_time_ms = metrics
                .response_times
                .iter()
                .map(|d| d.as_millis() as f64)
                .sum::<f64>()
                / metrics.response_times.len() as f64;

            metrics.weight = 1000.0 / (avg_time_ms + 1.0);
        }

        metrics.last_update = Instant::now();
    }

    pub fn record_failure(&self, addr: &Address) {
        let mut servers = self.servers.write();
        if let Some(metrics) = servers.get_mut(addr) {
            metrics.weight *= 0.8;
            metrics.last_update = Instant::now();
        }
    }

    fn select_by_response_time(&self) -> Option<Address> {
        let servers = self.servers.read();

        if servers.is_empty() {
            return None;
        }

        let now = Instant::now();
        let expired_servers: Vec<Address> = servers
            .iter()
            .filter(|(_, metrics)| {
                now.duration_since(metrics.last_update) > Duration::from_secs(300)
            })
            .map(|(addr, _)| addr.clone())
            .collect();

        drop(servers);

        if !expired_servers.is_empty() {
            let mut servers = self.servers.write();
            for addr in expired_servers {
                servers.remove(&addr);
            }
        }

        let servers = self.servers.read();

        let total_weight: f64 = servers.values().map(|m| m.weight).sum();
        if total_weight <= 0.0 {
            return servers.keys().next().cloned();
        }

        let mut rng = rand::rng();
        let mut random_weight = rng.random::<f64>() * total_weight;

        for (addr, metrics) in servers.iter() {
            random_weight -= metrics.weight;
            if random_weight <= 0.0 {
                return Some(addr.clone());
            }
        }

        servers.keys().next().cloned()
    }

    fn update_servers(&self, addresses: Vec<Address>) {
        let mut servers = self.servers.write();

        servers.retain(|addr, _| addresses.contains(addr));

        for addr in addresses {
            servers.entry(addr).or_insert_with(|| ServerMetrics {
                response_times: VecDeque::new(),
                last_update: Instant::now(),
                weight: 1.0,
                request_count: 0,
            });
        }
    }

    pub fn get_server_stats(&self) -> HashMap<Address, (f64, u64, Duration)> {
        let servers = self.servers.read();
        servers
            .iter()
            .map(|(addr, metrics)| {
                let avg_response_time = if metrics.response_times.is_empty() {
                    Duration::from_millis(0)
                } else {
                    let total_ms: u64 = metrics
                        .response_times
                        .iter()
                        .map(|d| d.as_millis() as u64)
                        .sum();
                    Duration::from_millis(total_ms / metrics.response_times.len() as u64)
                };

                (
                    addr.clone(),
                    (metrics.weight, metrics.request_count, avg_response_time),
                )
            })
            .collect()
    }
}

pub struct ResponseTimeWeightedIterator {
    balance: Arc<ResponseTimeWeightedBalance>,
    returned: bool,
}

impl Iterator for ResponseTimeWeightedIterator {
    type Item = Address;

    fn next(&mut self) -> Option<Self::Item> {
        if self.returned {
            return None;
        }
        self.returned = true;
        self.balance.select_by_response_time()
    }
}

impl<D> LoadBalance<D> for ResponseTimeWeightedBalance
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

            Ok(Box::new(ResponseTimeWeightedIterator {
                balance: Arc::new(self.clone()),
                returned: false,
            }) as Box<dyn Iterator<Item = Address> + Send>)
        })
    }

    fn rebalance(&self, _changes: Change<D::Key>) {}
}

impl Clone for ResponseTimeWeightedBalance {
    fn clone(&self) -> Self {
        Self {
            servers: Arc::clone(&self.servers),
            window_size: self.window_size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[test]
    fn test_record_response_time() {
        let balancer = ResponseTimeWeightedBalance::new(5);
        let addr = Address::from(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            8080,
        ));

        // Record some response times
        balancer.record_response_time(&addr, Duration::from_millis(100));
        balancer.record_response_time(&addr, Duration::from_millis(200));
        balancer.record_response_time(&addr, Duration::from_millis(150));

        let stats = balancer.get_server_stats();
        let (_weight, requests, avg_time) = stats.get(&addr).unwrap();

        assert_eq!(requests, &3);
        assert!(avg_time.as_millis() >= 150 && avg_time.as_millis() <= 151);
    }

    #[test]
    fn test_sliding_window() {
        let balancer = ResponseTimeWeightedBalance::new(3);
        let addr = Address::from(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            8080,
        ));

        // Fill window and overflow
        for i in 0..5 {
            balancer.record_response_time(&addr, Duration::from_millis(100 * (i + 1)));
        }

        let servers = balancer.servers.read();
        let metrics = servers.get(&addr).unwrap();
        assert_eq!(metrics.response_times.len(), 3); // Window size enforced
    }

    #[test]
    fn test_failure_handling() {
        let balancer = ResponseTimeWeightedBalance::new(5);
        let addr = Address::from(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            8080,
        ));

        balancer.record_response_time(&addr, Duration::from_millis(100));
        let initial_weight = balancer.get_server_stats().get(&addr).unwrap().0;

        balancer.record_failure(&addr);
        let after_failure_weight = balancer.get_server_stats().get(&addr).unwrap().0;

        assert!(after_failure_weight < initial_weight);
    }

    #[tokio::test]
    async fn test_server_selection() {
        let balancer = ResponseTimeWeightedBalance::new(5);

        // Add three servers with different response times
        let addrs: Vec<Address> = (8080..8083)
            .map(|port| {
                Address::from(SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                    port,
                ))
            })
            .collect();

        // Record different response times
        balancer.record_response_time(&addrs[0], Duration::from_millis(100));
        balancer.record_response_time(&addrs[1], Duration::from_millis(200));
        balancer.record_response_time(&addrs[2], Duration::from_millis(300));

        // Count selections over many iterations
        let mut selections = HashMap::new();
        for _ in 0..1000 {
            if let Some(selected) = balancer.select_by_response_time() {
                *selections.entry(selected).or_insert(0) += 1;
            }
        }

        // Faster servers should be selected more often
        assert!(selections.get(&addrs[0]).unwrap() > selections.get(&addrs[2]).unwrap());
    }

    #[test]
    fn test_server_expiration() {
        let balancer = ResponseTimeWeightedBalance::new(5);
        let addr = Address::from(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            8080,
        ));

        balancer.record_response_time(&addr, Duration::from_millis(100));

        // Manually set last_update to old timestamp
        {
            let mut servers = balancer.servers.write();
            if let Some(metrics) = servers.get_mut(&addr) {
                metrics.last_update = Instant::now() - Duration::from_secs(301);
            }
        }

        // Server should be removed after selection attempt
        balancer.select_by_response_time();
        assert!(balancer.servers.read().is_empty());
    }
}
