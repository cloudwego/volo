use std::array;

use tokio::sync::Mutex;

const SHARD_COUNT: usize = 64;

pub struct TxHashMap<T> {
    shared: [Mutex<fxhash::FxHashMap<i32, T>>; SHARD_COUNT],
}

impl<T> Default for TxHashMap<T> {
    fn default() -> Self {
        TxHashMap {
            shared: array::from_fn(|_| Default::default()),
        }
    }
}

impl<T> TxHashMap<T>
where
    T: Sized,
{
    pub async fn remove(&self, key: &i32) -> Option<T> {
        self.shared[(*key % (SHARD_COUNT as i32)) as usize]
            .lock()
            .await
            .remove(key)
    }

    pub async fn insert(&self, key: i32, value: T) -> Option<T> {
        self.shared[(key % (SHARD_COUNT as i32)) as usize]
            .lock()
            .await
            .insert(key, value)
    }

    pub async fn for_all_drain(&self, mut f: impl FnMut(T) -> ()) {
        for shared in self.shared.iter() {
            let mut s = shared.lock().await;
            for data in s.drain() {
                f(data.1)
            }
        }
    }
}
