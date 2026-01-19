use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use async_broadcast::Receiver;
use volo::{
    context::Endpoint,
    discovery::{Change, Discover, Instance},
    loadbalance::{
        LoadBalance,
        error::LoadBalanceError,
        least_conn::LeastConnectionBalance,
        p2c::P2c,
        random::WeightedRandomBalance,
        response_time_weighted::ResponseTimeWeightedBalance,
        round_robin::{RoundRobinBalance, WeightedRoundRobinBalance},
    },
    net::Address,
};

// Mock service discovery implementation
struct MockDiscover {
    instances: Vec<Arc<Instance>>,
}

impl MockDiscover {
    fn new(instances: Vec<Arc<Instance>>) -> Self {
        Self { instances }
    }
}

impl Discover for MockDiscover {
    type Key = String;
    type Error = LoadBalanceError;

    fn key(&self, _: &Endpoint) -> Self::Key {
        String::from("mock-discover")
    }

    async fn discover(&self, _: &Endpoint) -> Result<Vec<Arc<Instance>>, Self::Error> {
        Ok(self.instances.clone())
    }

    fn watch(&self, _: Option<&[String]>) -> Option<Receiver<Change<String>>> {
        None
    }
}

#[tokio::main]
async fn main() {
    // Create some test instances
    let instances = vec![
        Arc::new(Instance {
            address: Address::from(SocketAddr::from(([127, 0, 0, 1], 8080))),
            weight: 100,
            tags: HashMap::new(),
        }),
        Arc::new(Instance {
            address: Address::from(SocketAddr::from(([127, 0, 0, 1], 8081))),
            weight: 200,
            tags: HashMap::new(),
        }),
        Arc::new(Instance {
            address: Address::from(SocketAddr::from(([127, 0, 0, 1], 8082))),
            weight: 300,
            tags: HashMap::new(),
        }),
    ];

    let discover = MockDiscover::new(instances);
    let endpoint = Endpoint::new("test-service".into());

    // Example 1: Round Robin Load Balancer
    println!("\n=== Round Robin Load Balancer ===");
    let round_robin = RoundRobinBalance::new();
    demonstrate_lb(&round_robin, &discover, &endpoint).await;

    // Example 2: Weighted Round Robin Load Balancer
    println!("\n=== Weighted Round Robin Load Balancer ===");
    let weighted_round_robin = WeightedRoundRobinBalance::new(vec![]);
    demonstrate_lb(&weighted_round_robin, &discover, &endpoint).await;

    // Example 3: P2C (Power of Two Choices) Load Balancer
    println!("\n=== P2C Load Balancer ===");
    let p2c = P2c::default();
    demonstrate_lb(&p2c, &discover, &endpoint).await;

    // Example 4: Weighted Random Load Balancer
    println!("\n=== Weighted Random Load Balancer ===");
    let random = WeightedRandomBalance::new();
    demonstrate_lb(&random, &discover, &endpoint).await;

    // Example 5: Least Connection Load Balancer
    println!("\n=== Least Connection Load Balancer ===");
    let least_conn = LeastConnectionBalance::new();
    demonstrate_lb(&least_conn, &discover, &endpoint).await;

    // Example 6: Response Time Weighted Load Balancer
    println!("\n=== Response Time Weighted Load Balancer ===");
    let response_time = ResponseTimeWeightedBalance::new(100); // window size in items
    demonstrate_lb(&response_time, &discover, &endpoint).await;
}

async fn demonstrate_lb<L, D>(lb: &L, discover: &D, endpoint: &Endpoint)
where
    L: LoadBalance<D>,
    D: Discover,
{
    match lb.get_picker(endpoint, discover).await {
        Ok(mut picker) => {
            // Demonstrate 10 picks
            for i in 0..10 {
                if let Some(addr) = picker.next() {
                    println!("Pick {}: Selected instance: {}", i + 1, addr);
                }
                sleep(Duration::from_millis(100)).await;
            }
        }
        Err(e) => println!("Failed to get picker: {e:?}"),
    }
}
