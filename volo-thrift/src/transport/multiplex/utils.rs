use std::array;

use tokio::sync::Mutex;

const SHARD_COUNT: usize = 64;

pub struct TxHashMap<T> {
    sharded: [Mutex<ahash::HashMap<i32, T>>; SHARD_COUNT],
}

impl<T> Default for TxHashMap<T> {
    fn default() -> Self {
        TxHashMap {
            sharded: array::from_fn(|_| Default::default()),
        }
    }
}

impl<T> TxHashMap<T>
where
    T: Sized,
{
    pub async fn remove(&self, key: &i32) -> Option<T> {
        self.sharded[(*key % (SHARD_COUNT as i32)) as usize]
            .lock()
            .await
            .remove(key)
    }

    pub async fn is_empty(&self) -> bool {
        for s in self.sharded.iter() {
            if !s.lock().await.is_empty() {
                return false;
            }
        }
        true
    }

    pub async fn insert(&self, key: i32, value: T) -> Option<T> {
        self.sharded[(key % (SHARD_COUNT as i32)) as usize]
            .lock()
            .await
            .insert(key, value)
    }

    pub async fn for_all_drain(&self, mut f: impl FnMut(T) -> ()) {
        for sharded in self.sharded.iter() {
            let mut s = sharded.lock().await;
            for data in s.drain() {
                f(data.1)
            }
        }
    }
}
