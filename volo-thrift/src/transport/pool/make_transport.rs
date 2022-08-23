//! MakeTransport with pool

use std::{fmt::Debug, hash::Hash};

use futures::Future;
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
    MT: UnaryService<Key> + Send + Clone + 'static,
    MT::Response: Poolable + Send,
    MT::Error: Into<BoxError>,
{
    type Response = Pooled<Key, MT::Response>;

    type Error = BoxError;

    type Future<'cx> = impl Future<Output = Result<Self::Response, Self::Error>>;

    fn call(&mut self, key: Key) -> Self::Future<'_> {
        let mt = self.inner.clone();
        async move {
            let pool = self.pool.clone();
            pool.get(key, mt).await
        }
    }
}
