//! MakeTransport with pool

use motore::service::UnaryService;

use super::{Key, Pool, Poolable, Transport, Ver};

// pooled make transport wrap the inner MakeTransport and return the pooled transport
// when call make_transport
pub struct PooledMakeTransport<MT, K: Key>
where
    MT: UnaryService<K>,
    <MT as UnaryService<K>>::Response: Poolable,
{
    pub(crate) inner: MT,
    pub(crate) pool: Pool<K, MT::Response>,
}

impl<MT, K: Key> Clone for PooledMakeTransport<MT, K>
where
    MT: Clone,
    MT: UnaryService<K>,
    <MT as UnaryService<K>>::Response: Poolable,
{
    fn clone(&self) -> Self {
        PooledMakeTransport {
            inner: self.inner.clone(),
            pool: self.pool.clone(),
        }
    }
}

impl<MT, K: Key> PooledMakeTransport<MT, K>
where
    MT: UnaryService<K>,
    MT::Response: Poolable + Send + 'static,
{
    pub fn new(inner: MT, cfg: Option<super::Config>) -> Self {
        Self {
            inner,
            pool: Pool::new(cfg),
        }
    }
}

impl<MT, K: Key> UnaryService<(K, Option<K>, Ver)> for PooledMakeTransport<MT, K>
where
    MT: UnaryService<K> + Send + Clone + 'static + Sync,
    MT::Response: Poolable + Send,
    MT::Error: Into<crate::ClientError> + Send,
{
    type Response = Transport<K, MT::Response>;

    type Error = crate::ClientError;

    async fn call(&self, kv: (K, Option<K>, Ver)) -> Result<Self::Response, Self::Error> {
        let mt = self.inner.clone();
        if let Some(addr) = kv.1 {
            if let Ok(resp) = mt.call(addr.clone()).await {
                return Ok(Transport::UnPooled(resp));
            }
        }
        self.pool
            .get(kv.0, kv.2, mt)
            .await
            .map_err(Into::into)
            .map(Into::into)
    }
}
