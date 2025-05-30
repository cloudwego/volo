use std::{hash::Hash, sync::Arc};

use dashmap::{mapref::entry::Entry, DashMap};
use rand::Rng;

use super::{error::LoadBalanceError, LoadBalance};
use crate::{
    context::Endpoint,
    discovery::{Change, Discover, Instance},
    net::Address,
};

#[inline]
fn pick_one(
    sum_of_weight: usize,
    prefix_sum_of_weights: &[usize],
    instances: &[Arc<Instance>],
) -> Option<(usize, Arc<Instance>)> {
    if sum_of_weight == 0 {
        return None;
    }
    if instances.is_empty() {
        return None;
    }
    let weight = rand::rng().random_range(0..sum_of_weight);
    let index = prefix_sum_of_weights
        .binary_search(&weight)
        .unwrap_or_else(|index| index);
    Some((index, instances[index].clone()))
}

#[derive(Debug)]
pub struct InstancePicker {
    shared_instances: Arc<WeightedInstances>,
    sum_of_weights: usize,
    last_offset: Option<usize>,
    iter_times: usize,
}

impl Iterator for InstancePicker {
    type Item = Address;

    fn next(&mut self) -> Option<Self::Item> {
        let shared_instances = &self.shared_instances.instances;
        let prefix_sum_of_weights = &self.shared_instances.prefix_sum_of_weights;
        if shared_instances.is_empty() {
            return None;
        }
        self.iter_times += 1;
        match &mut self.last_offset {
            None => {
                let (offset, instance) =
                    pick_one(self.sum_of_weights, prefix_sum_of_weights, shared_instances)?;
                self.last_offset = Some(offset);
                Some(instance.address.clone())
            }
            Some(last_offset) => {
                if self.iter_times > shared_instances.len() {
                    return None;
                }
                let mut offset = *last_offset + 1;
                if offset == shared_instances.len() {
                    offset = 0;
                }
                *last_offset = offset;
                Some(shared_instances[offset].address.clone())
            }
        }
    }
}

#[derive(Debug, Clone)]
struct WeightedInstances {
    sum_of_weights: usize,
    prefix_sum_of_weights: Vec<usize>,
    instances: Vec<Arc<Instance>>,
}

impl From<Vec<Arc<Instance>>> for WeightedInstances {
    fn from(instances: Vec<Arc<Instance>>) -> Self {
        let mut sum_of_weights = 0;
        let mut prefix_sum_of_weights = Vec::with_capacity(instances.len());
        for instance in instances.iter() {
            sum_of_weights += instance.weight as usize;
            prefix_sum_of_weights.push(sum_of_weights);
        }
        Self {
            instances,
            prefix_sum_of_weights,
            sum_of_weights,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WeightedRandomBalance<K>
where
    K: Hash + PartialEq + Eq + Send + Sync + 'static,
{
    router: DashMap<K, Arc<WeightedInstances>>,
}

impl<K> WeightedRandomBalance<K>
where
    K: Hash + PartialEq + Eq + Send + Sync + 'static,
{
    pub fn with_discover<D>(_: &D) -> Self
    where
        D: Discover<Key = K>,
    {
        Self {
            router: DashMap::new(),
        }
    }

    pub fn new() -> Self {
        Self {
            router: DashMap::new(),
        }
    }
}

impl<D> LoadBalance<D> for WeightedRandomBalance<D::Key>
where
    D: Discover,
{
    type InstanceIter = InstancePicker;

    async fn get_picker<'future>(
        &'future self,
        endpoint: &'future Endpoint,
        discover: &'future D,
    ) -> Result<Self::InstanceIter, LoadBalanceError> {
        let key = discover.key(endpoint);
        let weighted_list = match self.router.entry(key) {
            Entry::Occupied(e) => e.get().clone(),
            Entry::Vacant(e) => {
                let instances = Arc::new(WeightedInstances::from(
                    discover
                        .discover(endpoint)
                        .await
                        .map_err(|err| err.into())?,
                ));
                e.insert(instances).value().clone()
            }
        };
        let sum_of_weights = weighted_list.sum_of_weights;
        Ok(InstancePicker {
            last_offset: None,
            iter_times: 0,
            shared_instances: weighted_list,
            sum_of_weights,
        })
    }

    fn rebalance(&self, changes: Change<D::Key>) {
        if let Entry::Occupied(entry) = self.router.entry(changes.key.clone()) {
            entry.replace_entry(Arc::new(WeightedInstances::from(changes.all)));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rand::{rng, RngCore};

    use super::{LoadBalance, WeightedRandomBalance};
    use crate::{
        context::Endpoint,
        discovery::{StaticDiscover, WeightedStaticDiscover},
    };

    #[tokio::test]
    async fn test_weighted_random() {
        let empty = Endpoint::new("".into());
        let discover = StaticDiscover::from(vec![
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.2:9000".parse().unwrap(),
        ]);
        let lb = WeightedRandomBalance::with_discover(&discover);
        let picker = lb.get_picker(&empty, &discover).await.unwrap();
        let all = picker.collect::<Vec<_>>();
        assert_eq!(all.len(), 2);
        assert_ne!(all[0], all[1]);
    }

    #[tokio::test]
    async fn test_weighted_random_load_balance() {
        let cycle = 10;

        let empty = Endpoint::new("".into());
        let mut weighted_instances = Vec::with_capacity(100);
        let mut total_weight = 0;
        for i in 0..100 {
            let addr = format!("127.0.0.{}:8000", i).parse().unwrap();
            let weight = rng().next_u32() % 100 + 1;
            weighted_instances.push((addr, weight));
            total_weight += weight;
        }
        let discover = WeightedStaticDiscover::from(weighted_instances.clone());
        let lb = WeightedRandomBalance::with_discover(&discover);

        let mut actual_weights: HashMap<String, u32> = HashMap::new();
        for _ in 0..(total_weight * cycle) {
            let mut picker = lb.get_picker(&empty, &discover).await.unwrap();
            let addr = picker.next().unwrap();
            let count = actual_weights.entry(addr.to_string()).or_insert(0);
            *count += 1;
        }
        for instance in weighted_instances.iter() {
            let addr = instance.0.to_string();
            let weight = instance.1;
            let count = *actual_weights.entry(addr.to_string()).or_insert(0);

            let expected_rate = (weight as f64) / (total_weight as f64);
            let actual_rate = (count as f64) / ((total_weight * cycle) as f64);

            println!(
                "addr: {}, expected: {}, actual: {}",
                addr, expected_rate, actual_rate
            );
            assert!((expected_rate - actual_rate).abs() < 0.01);
        }
    }
}
