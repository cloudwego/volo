use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{LoadBalance, error::LoadBalanceError};
use crate::{
    context::Endpoint,
    discovery::{Change, Discover},
    net::Address,
};
use futures::future::BoxFuture;

pub struct AdaptiveBalance {
    servers: Arc<RwLock<HashMap<Address, AdaptiveMetrics>>>,
    config: AdaptiveConfig,
}

#[derive(Debug, Clone)]
pub struct AdaptiveConfig {
    pub response_time_weight: f64,
    pub error_rate_weight: f64,
    pub active_conn_weight: f64,
    pub window_size: usize,
    pub min_requests: usize,
}

impl Default for AdaptiveConfig {
    fn default() -> Self {
        Self {
            response_time_weight: 0.4,
            error_rate_weight: 0.4,
            active_conn_weight: 0.2,
            window_size: 100,
            min_requests: 10,
        }
    }
}

#[derive(Debug)]
struct AdaptiveMetrics {
    /// Historical response times within the window
    response_times: Vec<Duration>,
    /// Average response time calculated from the window
    avg_response_time: Duration,

    /// Total number of requests processed
    total_requests: usize,
    /// Number of failed requests
    error_requests: usize,
    /// Calculated error rate (error_requests / total_requests)
    error_rate: f64,

    /// Number of current active connections
    active_connections: usize,

    /// Overall health score of the instance
    score: f64,
    /// Timestamp of the last metrics update
    last_update: Instant,
}

impl AdaptiveBalance {
    pub fn new(config: AdaptiveConfig) -> Self {
        Self {
            servers: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    pub fn with_default_config() -> Self {
        Self::new(AdaptiveConfig::default())
    }

    pub fn record_success(&self, addr: &Address, response_time: Duration) {
        let mut servers = self.servers.write();
        let metrics = servers
            .entry(addr.clone())
            .or_insert_with(|| AdaptiveMetrics {
                response_times: Vec::new(),
                avg_response_time: Duration::from_millis(0),
                total_requests: 0,
                error_requests: 0,
                error_rate: 0.0,
                active_connections: 0,
                score: 1.0,
                last_update: Instant::now(),
            });

        metrics.response_times.push(response_time);
        if metrics.response_times.len() > self.config.window_size {
            metrics.response_times.remove(0);
        }

        let total_ms: u64 = metrics
            .response_times
            .iter()
            .map(|d| d.as_millis() as u64)
            .sum();
        metrics.avg_response_time =
            Duration::from_millis(total_ms / metrics.response_times.len() as u64);

        metrics.total_requests += 1;

        if metrics.total_requests > 0 {
            metrics.error_rate = metrics.error_requests as f64 / metrics.total_requests as f64;
        }

        metrics.last_update = Instant::now();
        self.calculate_score(metrics);
    }

    pub fn record_failure(&self, addr: &Address, response_time: Option<Duration>) {
        let mut servers = self.servers.write();
        let metrics = servers
            .entry(addr.clone())
            .or_insert_with(|| AdaptiveMetrics {
                response_times: Vec::new(),
                avg_response_time: Duration::from_millis(0),
                total_requests: 0,
                error_requests: 0,
                error_rate: 0.0,
                active_connections: 0,
                score: 1.0,
                last_update: Instant::now(),
            });

        if let Some(rt) = response_time {
            metrics.response_times.push(rt);
            if metrics.response_times.len() > self.config.window_size {
                metrics.response_times.remove(0);
            }
        }

        metrics.total_requests += 1;
        metrics.error_requests += 1;
        metrics.error_rate = metrics.error_requests as f64 / metrics.total_requests as f64;

        metrics.last_update = Instant::now();
        self.calculate_score(metrics);
    }

    pub fn on_connection_start(&self, addr: &Address) {
        let mut servers = self.servers.write();
        if let Some(metrics) = servers.get_mut(addr) {
            metrics.active_connections += 1;
            self.calculate_score(metrics);
        }
    }

    pub fn on_connection_end(&self, addr: &Address) {
        let mut servers = self.servers.write();
        if let Some(metrics) = servers.get_mut(addr) {
            metrics.active_connections = metrics.active_connections.saturating_sub(1);
            self.calculate_score(metrics);
        }
    }

    fn calculate_score(&self, metrics: &mut AdaptiveMetrics) {
        if metrics.total_requests < self.config.min_requests {
            metrics.score = 1.0;
            return;
        }

        let response_score = if metrics.avg_response_time.as_millis() > 0 {
            1000.0 / (metrics.avg_response_time.as_millis() as f64 + 1.0)
        } else {
            1.0
        };

        let error_score = 1.0 - metrics.error_rate;

        let conn_score = 1.0 / (metrics.active_connections as f64 + 1.0);

        metrics.score = response_score * self.config.response_time_weight
            + error_score * self.config.error_rate_weight
            + conn_score * self.config.active_conn_weight;

        metrics.score = metrics.score.clamp(0.01, 10.0);
    }

    fn select_best_server(&self) -> Option<Address> {
        let servers = self.servers.read();

        if servers.is_empty() {
            return None;
        }

        servers
            .iter()
            .max_by(|(_, a), (_, b)| a.score.partial_cmp(&b.score).unwrap())
            .map(|(addr, _)| addr.clone())
    }

    fn update_servers(&self, addresses: Vec<Address>) {
        let mut servers = self.servers.write();

        servers.retain(|addr, _| addresses.contains(addr));

        for addr in addresses {
            servers.entry(addr).or_insert_with(|| AdaptiveMetrics {
                response_times: Vec::new(),
                avg_response_time: Duration::from_millis(0),
                total_requests: 0,
                error_requests: 0,
                error_rate: 0.0,
                active_connections: 0,
                score: 1.0,
                last_update: Instant::now(),
            });
        }
    }

    pub fn get_server_stats(&self) -> HashMap<Address, (f64, f64, Duration, usize)> {
        let servers = self.servers.read();
        servers
            .iter()
            .map(|(addr, metrics)| {
                (
                    addr.clone(),
                    (
                        metrics.score,
                        metrics.error_rate,
                        metrics.avg_response_time,
                        metrics.active_connections,
                    ),
                )
            })
            .collect()
    }
}

pub struct AdaptiveIterator {
    balance: Arc<AdaptiveBalance>,
    returned: bool,
}

impl Iterator for AdaptiveIterator {
    type Item = Address;

    fn next(&mut self) -> Option<Self::Item> {
        if self.returned {
            return None;
        }
        self.returned = true;

        let addr = self.balance.select_best_server();
        if let Some(ref address) = addr {
            self.balance.on_connection_start(address);
        }
        addr
    }
}

impl<D> LoadBalance<D> for AdaptiveBalance
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

            Ok(Box::new(AdaptiveIterator {
                balance: Arc::new(self.clone()),
                returned: false,
            }) as Box<dyn Iterator<Item = Address> + Send>)
        })
    }

    fn rebalance(&self, _changes: Change<D::Key>) {}
}

impl Clone for AdaptiveBalance {
    fn clone(&self) -> Self {
        Self {
            servers: Arc::clone(&self.servers),
            config: self.config.clone(),
        }
    }
}
