use std::{cmp::min, collections::HashSet, hash::Hash, sync::Arc};

use dashmap::{DashMap, mapref::entry::Entry};
use futures::future::BoxFuture;

use super::{LoadBalance, RequestHash, error::LoadBalanceError};
use crate::{
    context::Endpoint,
    discovery::{Change, Discover, Instance},
    net::Address,
};

#[derive(Debug, Clone)]
pub struct ConsistentHashOption {
    /// If it is set to a value greater than 1, replicas will be used when connect to the primary
    /// node fails. This brings extra mem and cpu cost.
    /// If it is set to 1, error will be returned immediately when connect fails.
    replicas: usize,

    /// The number of virtual nodes corresponding to each real node
    /// The larger the value, the higher the memory and computational cost, and the more balanced
    /// the load When the number of nodes is large, it can be set smaller; conversely, it can be
    /// set larger The median VirtualFactor * Weight (if Weighted is true) is recommended to be
    /// around 1000 The recommended total number of virtual nodes is within 2000W
    virtual_factor: u32,

    /// Whether to follow Weight for load balancing
    /// If false, Weight is ignored for each instance, and VirtualFactor virtual nodes are
    /// generated for indiscriminate load balancing if true, Weight() * VirtualFactor virtual
    /// nodes are generated for each instance Note that for instance with weight 0, no virtual
    /// nodes will be generated regardless of the VirtualFactor number It is recommended to set
    /// it to true, but be careful to reduce the VirtualFactor appropriately
    weighted: bool,
}

impl ConsistentHashOption {
    pub fn new(replicas: usize, virtual_factor: u32, weighted: bool) -> Self {
        ConsistentHashOption {
            replicas,
            virtual_factor,
            weighted,
        }
    }
}

impl Default for ConsistentHashOption {
    fn default() -> Self {
        ConsistentHashOption {
            replicas: 1,
            virtual_factor: 100,
            weighted: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// RealNode is a wrapper of Instance
struct RealNode(Instance);

impl From<Instance> for RealNode {
    fn from(instance: Instance) -> Self {
        RealNode(instance)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// VirtualNode contains a RealNode and a hash value(which is get by hashing the RealNode's address
/// and serial number), and the virtualnode will be sorted by the hash value.
struct VirtualNode {
    real_node: Arc<RealNode>,
    hash: u64,
}

impl PartialOrd for VirtualNode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for VirtualNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.hash.cmp(&other.hash)
    }
}

#[derive(Debug, Clone)]
struct WeightedInstances {
    real_nodes: Vec<Arc<RealNode>>,
    virtual_nodes: Vec<VirtualNode>,
}

#[derive(Debug)]
pub struct InstancePicker {
    shared_instances: Arc<WeightedInstances>,

    /// used for searching the virtual node
    request_hash: RequestHash,

    /// The index of the last selected virtual node
    last_pick: Option<usize>,

    /// The set of realnodes that have been selected
    used: HashSet<Address>,

    /// The number of replicas to pick, min(option.replicas, real_nodes.len())
    replicas: usize,
}

impl Iterator for InstancePicker {
    type Item = Address;

    fn next(&mut self) -> Option<Self::Item> {
        let virtual_nodes = &self.shared_instances.virtual_nodes;
        if self.shared_instances.real_nodes.is_empty() {
            return None;
        }

        // already picked all replicas
        if self.used.len() >= self.replicas {
            return None;
        }

        match self.last_pick {
            None => {
                // init states
                self.replicas = min(self.replicas, self.shared_instances.real_nodes.len());
                // find the first virtual node whose hash is greater than request_hash
                let mut index = virtual_nodes.partition_point(|vn| vn.hash < self.request_hash.0);
                if index == virtual_nodes.len() {
                    index = 0;
                }
                self.last_pick = Some(index);
                let addr = virtual_nodes
                    .get(index)
                    .unwrap()
                    .real_node
                    .0
                    .address
                    .clone();
                self.used.insert(addr.clone());
                Some(addr)
            }
            Some(last_pick) => {
                let mut index = last_pick;
                // find the next virtual node which is not used
                for _ in 0..virtual_nodes.len() {
                    index += 1;
                    if index == virtual_nodes.len() {
                        index = 0;
                    }
                    let addr = &virtual_nodes.get(index).unwrap().real_node.0.address;
                    if !self.used.contains(addr) {
                        self.last_pick = Some(index);
                        self.used.insert(addr.clone());
                        return Some(addr.clone());
                    }
                }
                None
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConsistentHashBalance<K>
where
    K: Hash + PartialEq + Eq + Send + Sync + 'static,
{
    option: ConsistentHashOption,
    router: DashMap<K, Arc<WeightedInstances>>,
}

impl<K> ConsistentHashBalance<K>
where
    K: Hash + PartialEq + Eq + Send + Sync + 'static,
{
    pub fn with_discover<D>(&mut self, _: &D) -> &mut Self
    where
        D: Discover<Key = K>,
    {
        self
    }

    pub fn new(option: ConsistentHashOption) -> Self {
        Self {
            option,
            router: DashMap::new(),
        }
    }

    fn build_weighted_instances(&self, instances: Vec<Arc<Instance>>) -> WeightedInstances {
        let mut real_nodes = Vec::with_capacity(instances.len());
        // total number of virtual nodes
        let virtual_factor = self.option.virtual_factor;
        let sum_of_nodes = if self.option.weighted {
            instances
                .iter()
                .fold(0, |lhs, rhs| lhs + (rhs.weight * virtual_factor) as usize)
        } else {
            instances.len() * virtual_factor as usize
        };
        let mut virtual_nodes = Vec::with_capacity(sum_of_nodes);
        for instance in instances {
            let real_node = Arc::new(RealNode::from((*instance).clone()));
            real_nodes.push(real_node.clone());
            let mut weight = 1;
            if self.option.weighted {
                weight = instance.weight;
            }
            let str = instance.address.to_string();
            let vnode_lens = virtual_factor * weight;
            // try to reuse the buffer
            let mut buf = format!("{str}#{vnode_lens}").into_bytes();
            let mut sharp_pos = 0;
            for (i, bytei) in buf.iter().enumerate() {
                if *bytei == b'#' {
                    sharp_pos = i;
                    break;
                }
            }
            for i in 0..(virtual_factor * weight) {
                let mut serial = i;
                let mut pos = buf.len();
                while serial > 0 {
                    pos -= 1;
                    buf[pos] = b'0' + (serial % 10) as u8;
                    serial /= 10;
                }
                for bytej in buf.iter_mut().take(pos).skip(sharp_pos + 1) {
                    *bytej = b'0';
                }
                // get address#i with leading zeros
                let hash = mur3::murmurhash3_x64_128(&buf, 0).0;
                virtual_nodes.push(VirtualNode {
                    real_node: real_node.clone(),
                    hash,
                });
            }
        }
        virtual_nodes.sort_unstable();
        WeightedInstances {
            real_nodes,
            virtual_nodes,
        }
    }
}

impl<D> LoadBalance<D> for ConsistentHashBalance<D::Key>
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
            let request_hash = metainfo::METAINFO
                .try_with(|m| m.borrow().get::<RequestHash>().copied())
                .map_err(|_| LoadBalanceError::MissRequestHash)?;
            if request_hash.is_none() {
                return Err(LoadBalanceError::MissRequestHash);
            }
            let request_hash = request_hash.unwrap();
            let key = discover.key(endpoint);
            let weighted_list = if let Some(instances) = self.router.get(&key) {
                instances.clone()
            } else {
                let instances = Arc::new(
                    self.build_weighted_instances(
                        discover
                            .discover(endpoint)
                            .await
                            .map_err(|err| err.into())?,
                    ),
                );
                self.router.insert(key, Arc::clone(&instances));
                instances
            };
            Ok(Box::new(InstancePicker {
                shared_instances: weighted_list,
                request_hash,
                last_pick: None,
                used: HashSet::new(),
                replicas: self.option.replicas,
            }) as Box<dyn Iterator<Item = Address> + Send>)
        })
    }

    fn rebalance(&self, changes: Change<<D as Discover>::Key>) {
        if let Entry::Occupied(entry) = self.router.entry(changes.key.clone()) {
            entry.replace_entry(Arc::new(self.build_weighted_instances(changes.all)));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        collections::{BTreeSet, HashMap},
        sync::Arc,
    };

    use futures::Future;
    use metainfo::{METAINFO, MetaInfo};
    use rand::Rng;

    use super::{ConsistentHashBalance, ConsistentHashOption, LoadBalance};
    use crate::{
        context::Endpoint,
        discovery::{Instance, StaticDiscover},
        loadbalance::RequestHash,
        net::Address,
    };

    fn empty_endpoint() -> Endpoint {
        Endpoint::new("".into())
    }

    #[inline]
    fn set_request_hash(code: u64) {
        metainfo::METAINFO
            .try_with(|m| m.borrow_mut().insert(RequestHash(code)))
            .unwrap();
    }

    async fn test_with_meta_info<F, Fut>(f: F)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ()>,
    {
        let mi = MetaInfo::new();
        METAINFO
            .scope(RefCell::new(mi), async move {
                f().await;
            })
            .await;
    }

    #[tokio::test]
    async fn test_consistent_hash_balancer() {
        test_with_meta_info(consistent_hash_balancer_tests).await;
    }

    async fn consistent_hash_balancer_tests() {
        let empty = empty_endpoint();
        let discover = StaticDiscover::from(vec![
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.2:9000".parse().unwrap(),
        ]);
        let opt = ConsistentHashOption {
            replicas: 3,
            virtual_factor: 3,
            weighted: true,
        };
        let lb = ConsistentHashBalance::new(opt);
        set_request_hash(0);
        let picker = lb.get_picker(&empty, &discover).await.unwrap();
        let all = picker.collect::<Vec<_>>();
        assert_eq!(all.len(), 2);
        assert_ne!(all[0], all[1]);

        // Test replicas in ConsistentHashOption
        let opt = ConsistentHashOption {
            replicas: 1,
            virtual_factor: 3,
            weighted: true,
        };
        let lb = ConsistentHashBalance::new(opt);
        set_request_hash(0);
        let picker = lb.get_picker(&empty, &discover).await.unwrap();
        let all = picker.collect::<Vec<_>>();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn test_consistent_hash_consistent() {
        test_with_meta_info(consistent_hash_consistent_tests).await;
    }

    async fn consistent_hash_consistent_tests() {
        // test if the same request key will get the same instances
        let empty = empty_endpoint();
        let instances = vec![
            Arc::new(Instance {
                address: Address::Ip("127.0.0.1:8000".parse().unwrap()),
                weight: 3,
                tags: Default::default(),
            }),
            Arc::new(Instance {
                address: Address::Ip("127.0.0.2:9000".parse().unwrap()),
                weight: 3,
                tags: Default::default(),
            }),
            Arc::new(Instance {
                address: Address::Ip("127.0.0.3:8800".parse().unwrap()),
                weight: 3,
                tags: Default::default(),
            }),
        ];
        let sum_weight = instances.iter().map(|i| i.weight).sum::<u32>();
        let opt = ConsistentHashOption {
            replicas: 2,
            virtual_factor: 3,
            weighted: true,
        };
        let discovery = StaticDiscover::new(instances.clone());
        let lb = ConsistentHashBalance::new(opt.clone());
        let weighted_instances = lb.build_weighted_instances(instances.clone());
        assert_eq!(
            weighted_instances.virtual_nodes.len(),
            (sum_weight * opt.virtual_factor) as usize
        );
        assert_eq!(weighted_instances.real_nodes.len(), instances.len());
        for _ in 0..100 {
            let request_hash = rand::random::<u64>();
            set_request_hash(request_hash);
            let picker = lb.get_picker(&empty, &discovery).await.unwrap();
            let all1 = picker.collect::<Vec<_>>();
            for _ in 0..3 {
                let picker2 = lb.get_picker(&empty, &discovery).await.unwrap();
                let all2 = picker2.collect::<Vec<_>>();
                assert_eq!(all1, all2);
            }
        }
    }

    async fn simulate_random_picks(
        instances: Vec<Arc<Instance>>,
        times: usize,
    ) -> HashMap<Address, usize> {
        let mut map = HashMap::new();
        let empty = empty_endpoint();
        let opt = ConsistentHashOption {
            replicas: 3,
            virtual_factor: 100,
            weighted: true,
        };
        let discovery = StaticDiscover::new(instances.clone());
        let lb = ConsistentHashBalance::new(opt.clone());
        for _ in 0..times {
            let request_hash = rand::random::<u64>();
            set_request_hash(request_hash);
            let picker = lb.get_picker(&empty, &discovery).await.unwrap();
            let all = picker.collect::<Vec<_>>();
            for address in all {
                let count = map.entry(address).or_insert(0);
                *count += 1;
            }
        }
        map
    }

    fn new_instance(address: String, weight: u32) -> Arc<Instance> {
        Arc::new(Instance {
            address: Address::Ip(address.parse().unwrap()),
            weight,
            tags: Default::default(),
        })
    }

    #[tokio::test]
    async fn test_consistent_hash_balance() {
        test_with_meta_info(consistent_hash_balance_tests).await;
    }

    async fn consistent_hash_balance_tests() {
        // TODO: Using standard deviation to evaluate load balancing is better?
        let mut rng = rand::rng();
        let mut instances = vec![];
        for _ in 0..50 {
            let w = rng.random_range(10..=100);
            let sub_net = rng.random_range(0..=255);
            let port = rng.random_range(1000..=65535);
            instances.push(new_instance(format!("172.17.0.{sub_net}:{port}"), w));
            instances.push(new_instance(format!("192.168.32.{sub_net}:{port}"), w));
        }
        let result = simulate_random_picks(instances.clone(), 1000000).await;
        let sum_visits = result.values().sum::<usize>();
        let sum_weight = instances.iter().map(|i| i.weight).sum::<u32>();
        let mut deviation = 0.0;
        let mut max_eps: f64 = 0.0;
        for instance in instances.iter() {
            let count: usize = *(result.get(&(instance.address)).unwrap_or(&0));
            let exact = count as f64;
            let expect = instance.weight as f64 / sum_weight as f64 * sum_visits as f64;
            let eps = (exact - expect).abs() / expect;
            // compute the standard deviation
            deviation += eps * eps;
            max_eps = max_eps.max(eps);
        }
        println!("max_eps: {max_eps}");
        println!(
            "standard deviation: {}",
            (deviation / instances.len() as f64).sqrt()
        );
        assert!(max_eps < 0.1);
    }

    #[tokio::test]
    async fn test_consistent_hash_change() {
        test_with_meta_info(consistent_hash_change_tests).await;
    }

    async fn consistent_hash_change_tests() {
        let mut instances = vec![];
        let opt = ConsistentHashOption {
            replicas: 1,
            virtual_factor: 100,
            weighted: true,
        };
        let mut rng = rand::rng();
        for i in 0..30 {
            let w = rng.random_range(10..=100);
            instances.push(new_instance(format!("127.0.0.1:{i}"), w));
        }
        let discovery = StaticDiscover::new(instances.clone());
        let mut lb = ConsistentHashBalance::new(opt.clone());
        lb.with_discover(&discovery);
        let virtual_nodes = lb.build_weighted_instances(instances.clone()).virtual_nodes;
        let virtual_nodes: BTreeSet<_> = virtual_nodes.into_iter().collect();

        let remove_index = rng.random_range(0..instances.len());
        let _remove_instance = instances.remove(remove_index);
        let new_virtual_nodes = lb.build_weighted_instances(instances.clone()).virtual_nodes;
        for node in new_virtual_nodes {
            assert!(virtual_nodes.contains(&node));
        }
    }
}
