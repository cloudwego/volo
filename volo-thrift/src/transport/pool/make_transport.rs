//! MakeTransport with pool

use std::{fmt::Debug, hash::Hash};

use motore::{service::UnaryService, BoxError};

use super::{Pool, Poolable, Pooled};

// pooled make transport wrap the inner MakeTransport and return the pooled transport
// when call make_transport
pub struct PooledMakeTransport<MT, Key>
where
    MT: UnaryService<Key>,
{
    pub(crate) inner: MT,
    pub(crate) pool: Pool<Key, MT::Response>,
}

impl<MT, Key> Clone for PooledMakeTransport<MT, Key>
where
    MT: Clone,
    MT: UnaryService<Key>,
{
    fn clone(&self) -> Self {
        PooledMakeTransport {
            inner: self.inner.clone(),
            pool: self.pool.clone(),
        }
    }
}

impl<MT, Key> PooledMakeTransport<MT, Key>
where
    MT: UnaryService<Key>,
    MT::Response: Poolable + Send + 'static,
    Key: Clone + Eq + Hash + Debug + Send + 'static,
{
    pub fn new(inner: MT, cfg: Option<super::Config>) -> Self {
        Self {
            inner,
            pool: Pool::new(cfg),
        }
    }
}

impl<MT, Key> UnaryService<Key> for PooledMakeTransport<MT, Key>
where
    Key: Clone + Eq + Hash + Debug + Send + 'static,
    MT: UnaryService<Key> + Send + Clone + 'static + Sync,
    MT::Response: Poolable + Send,
    MT::Error: Into<BoxError>,
{
    type Response = Pooled<Key, MT::Response>;

    type Error = BoxError;

    async fn call(&self, key: Key) -> Result<Self::Response, Self::Error> {
        let mt = self.inner.clone();
        self.pool.get(key, mt).await
    }
}
