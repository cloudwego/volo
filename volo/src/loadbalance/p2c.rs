use futures::future::BoxFuture;
use rand::Rng;
use std::marker::PhantomData;

use super::{LoadBalance, error::LoadBalanceError};
use crate::{
    context::Endpoint,
    discovery::{Change, Discover, Instance},
    net::Address,
};

/// P2C (Power of Two Choices) load balancer that selects the better of two random choices
/// based on their current load.
pub struct P2c<D: Discover> {
    _phantom: PhantomData<D>,
}

impl<D: Discover> Default for P2c<D> {
    fn default() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

/// Calculate the load score for an instance based on its weight
///
/// For P2C load balancing, we want to consider both the weight and some randomness
/// to ensure proper distribution while still respecting weights
fn score_instance(instance: &Instance) -> f64 {
    let weight = instance.weight as f64;
    // Add some randomness to the score while still maintaining weight preference
    let random_factor = rand::random::<f64>();
    weight * (0.5 + 0.5 * random_factor) // Score will be between 50% and 100% of the weight
}

use std::sync::Arc;

pub struct P2cInstanceIter {
    instances: Vec<Arc<Instance>>,
}

impl Iterator for P2cInstanceIter {
    type Item = Address;

    fn next(&mut self) -> Option<Self::Item> {
        if self.instances.is_empty() {
            return None;
        }

        if self.instances.len() == 1 {
            return Some(self.instances[0].address.clone());
        }

        // Pick two random instances
        let mut rng = rand::rng();
        let first_idx = rng.random_range(0..self.instances.len());
        let mut second_idx = rng.random_range(0..self.instances.len() - 1);
        if second_idx >= first_idx {
            second_idx += 1;
        }

        let first = &self.instances[first_idx];
        let second = &self.instances[second_idx];

        let first_weight = score_instance(first);
        let second_weight = score_instance(second);

        // Use probability to determine which instance to choose
        let total_score = first_weight + second_weight;
        let threshold = first_weight / total_score;

        // Generate a random number between 0 and 1 to decide which instance to select
        if rand::random::<f64>() < threshold {
            Some(first.address.clone())
        } else {
            Some(second.address.clone())
        }
    }
}

impl<D: Discover> LoadBalance<D> for P2c<D> {
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
            if instances.is_empty() {
                return Err(LoadBalanceError::NoAvailableInstance);
            }

            Ok(Box::new(P2cInstanceIter {
                instances: instances.into_iter().collect(),
            }) as Box<dyn Iterator<Item = Address> + Send>)
        })
    }

    fn rebalance(&self, _changes: Change<D::Key>) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::ready;
    use std::{collections::HashMap, net::SocketAddr};

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

    #[tokio::test]
    async fn test_empty_instances() {
        let p2c = P2c::<TestDiscover>::default();
        let discover = TestDiscover::default();
        let endpoint = Endpoint::new("test".into());

        let result = p2c.get_picker(&endpoint, &discover).await;
        assert!(matches!(result, Err(LoadBalanceError::NoAvailableInstance)));
    }

    #[tokio::test]
    async fn test_single_instance() {
        let p2c = P2c::<TestDiscover>::default();
        let mut discover = TestDiscover::default();

        let socket = SocketAddr::from(([127, 0, 0, 1], 8080));
        let addr = Address::from(socket);
        let instance = Arc::new(Instance {
            address: addr.clone(),
            weight: 100,
            tags: HashMap::new(),
        });
        discover.instances.push(instance);

        let endpoint = Endpoint::new("test".into());
        let result = p2c.get_picker(&endpoint, &discover).await.unwrap();

        // Single instance should always return that instance
        let picked_addr = result.take(1).next().unwrap();
        assert_eq!(picked_addr, addr);
    }

    #[tokio::test]
    async fn test_multiple_instances() {
        let p2c = P2c::<TestDiscover>::default();
        let mut discover = TestDiscover::default();

        // Create two instances with different weights
        let socket1 = SocketAddr::from(([127, 0, 0, 1], 8080));
        let socket2 = SocketAddr::from(([127, 0, 0, 1], 8081));
        let addr1 = Address::from(socket1);
        let addr2 = Address::from(socket2);

        let instance1 = Arc::new(Instance {
            address: addr1.clone(),
            weight: 50, // Lower weight
            tags: HashMap::new(),
        });
        let instance2 = Arc::new(Instance {
            address: addr2.clone(),
            weight: 100, // Higher weight
            tags: HashMap::new(),
        });

        discover.instances.extend(vec![instance1, instance2]);

        let endpoint = Endpoint::new("test".into());
        let mut result = p2c.get_picker(&endpoint, &discover).await.unwrap();

        // Due to the randomness of P2C algorithm, we need multiple selections to verify distribution
        let mut addr1_count = 0;
        let mut addr2_count = 0;

        // Increase the number of selections to reduce the impact of randomness
        for _ in 0..1000 {
            let addr = result.next().unwrap();
            if addr == addr1 {
                addr1_count += 1;
            } else if addr == addr2 {
                addr2_count += 1;
            }
        }

        // Verify that both addresses were selected at least once
        assert!(addr1_count > 0, "addr1 was never selected");
        assert!(addr2_count > 0, "addr2 was never selected");

        // Due to weight difference, addr2 should be selected more often
        assert!(
            addr2_count > addr1_count,
            "Expected addr2 (weight 100) to be selected more than addr1 (weight 50)"
        );
    }

    #[test]
    fn test_score_calculation() {
        let socket = SocketAddr::from(([127, 0, 0, 1], 8080));
        let instance = Instance {
            address: Address::from(socket),
            weight: 100,
            tags: HashMap::new(),
        };

        let score = score_instance(&instance);
        // Since the score now includes a random factor, we test if it falls within the expected range
        assert!(
            (50.0..=100.0).contains(&score),
            "Score should be between 50 and 100"
        );
        assert!(score != 0.0, "Score should not be zero");
    }
}
