use std::collections::{hash_map::Iter, HashMap};

use ahash::RandomState;
use bytes::{BufMut, BytesMut};
use faststr::FastStr;

#[derive(Clone, Debug, Default)]
pub struct Params {
    inner: HashMap<FastStr, FastStr, RandomState>,
}

impl Params {
    pub(crate) fn extend(&mut self, params: matchit::Params<'_, '_>) {
        self.inner.reserve(params.len());

        let cap = params.iter().map(|(k, v)| k.len() + v.len()).sum();
        let mut buf = BytesMut::with_capacity(cap);

        for (k, v) in params.iter() {
            buf.put(k.as_bytes());
            // SAFETY: The key is from a valid string as path of router
            let k = unsafe { FastStr::from_bytes_unchecked(buf.split().freeze()) };
            buf.put(v.as_bytes());
            // SAFETY: The value is from a valid string as requested uri
            let v = unsafe { FastStr::from_bytes_unchecked(buf.split().freeze()) };
            if self.inner.insert(k, v).is_some() {
                tracing::info!("[VOLO-HTTP] Conflicting key in param");
            }
        }
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> Iter<'_, FastStr, FastStr> {
        self.inner.iter()
    }

    pub fn get<K: Into<FastStr>>(&self, k: K) -> Option<&FastStr> {
        self.inner.get(&k.into())
    }
}
