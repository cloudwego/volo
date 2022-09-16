use core::cell::OnceCell;
use std::{future::Future, hash::Hash, sync::Arc};

use dashmap::{mapref::entry::Entry, DashMap};
use rand::Rng;

use super::{error::LoadBalanceError, LoadBalance};
use crate::{
    context::Endpoint,
    discovery::{Change, Discover, Instance},
    net::Address,
};

#[inline]
fn pick_one(weight: isize, iter: &[Arc<Instance>]) -> Option<(usize, Arc<Instance>)> {
    if weight == 0 {
        return None;
    }
    let mut weight = rand::thread_rng().gen_range(0..weight);
    for (offset, instance) in iter.iter().enumerate() {
        weight -= instance.weight as isize;
        if weight <= 0 {
            return Some((offset, instance.clone()));
        }
    }
    None
}

#[derive(Debug)]
pub struct InstancePicker {
    shared_instances: Arc<WeightedInstances>,
    sum_of_weights: isize,
    owned_instances: OnceCell<Vec<Arc<Instance>>>,
    last_pick: Option<(usize, Arc<Instance>)>,
}

impl Iterator for InstancePicker {
    type Item = Address;

    fn next(&mut self) -> Option<Self::Item> {
        let shared_instances = &self.shared_instances.instances;
        if shared_instances.is_empty() {
            return None;
        }

        match &mut self.last_pick {
            None => {
                let (offset, instance) = pick_one(self.sum_of_weights, shared_instances)?;
                self.last_pick = Some((offset, instance.clone()));
                Some(instance.address.clone())
            }
            Some((last_offset, last_pick)) => {
                self.owned_instances
                    .get_or_init(|| shared_instances.to_vec());
                let owned = self.owned_instances.get_mut().unwrap();

                self.sum_of_weights -= last_pick.weight as isize;
                owned.remove(*last_offset);

                (*last_offset, *last_pick) = pick_one(self.sum_of_weights, owned)?;

                Some(last_pick.clone().address.clone())
            }
        }
    }
}

#[derive(Debug, Clone)]
struct WeightedInstances {
    sum_of_weights: isize,
    instances: Vec<Arc<Instance>>,
}

impl From<Vec<Arc<Instance>>> for WeightedInstances {
    fn from(instances: Vec<Arc<Instance>>) -> Self {
        let sum_of_weights = instances
            .iter()
            .fold(0, |lhs, rhs| lhs + rhs.weight as isize);
        Self {
            instances,
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
    type InstanceIter<'iter> = InstancePicker;

    type GetFut<'future, 'iter> =
        impl Future<Output = Result<Self::InstanceIter<'iter>, LoadBalanceError>> + Send;

    fn get_picker<'future, 'iter>(
        &'iter self,
        endpoint: &'future Endpoint,
        discover: &'future D,
    ) -> Self::GetFut<'future, 'iter> {
        async {
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
                owned_instances: OnceCell::new(),
                last_pick: None,
                shared_instances: weighted_list,
                sum_of_weights,
            })
        }
    }

    fn rebalance(&self, changes: Change<D::Key>) {
        if let Entry::Occupied(entry) = self.router.entry(changes.key.clone()) {
            entry.replace_entry(Arc::new(WeightedInstances::from(changes.all)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LoadBalance, WeightedRandomBalance};
    use crate::{context::Endpoint, discovery::StaticDiscover};

    #[tokio::test]
    async fn test_weighted_random() {
        let empty = Endpoint {
            service_name: smol_str::SmolStr::new_inline(""),
            address: None,
            tags: Default::default(),
        };
        let discover = StaticDiscover::from(vec![
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.2:9000".parse().unwrap(),
        ]);
        let lb = WeightedRandomBalance::with_discover(&discover);
        let picker = lb.get_picker(&empty, &discover).await.unwrap();
        let all = picker.collect::<Vec<_>>();
        assert!(all.len() == 2);
        assert!(all[0] != all[1]);
    }
}
