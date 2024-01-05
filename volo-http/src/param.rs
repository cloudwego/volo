use std::slice::Iter;

use bytes::{BufMut, Bytes, BytesMut};

#[derive(Clone, Debug)]
pub struct Params {
    pub(crate) inner: Vec<(Bytes, Bytes)>,
}

impl From<matchit::Params<'_, '_>> for Params {
    fn from(params: matchit::Params) -> Self {
        let mut inner = Vec::with_capacity(params.len());
        let mut capacity = 0;
        for (k, v) in params.iter() {
            capacity += k.len();
            capacity += v.len();
        }

        let mut buf = BytesMut::with_capacity(capacity);

        for (k, v) in params.iter() {
            buf.put(k.as_bytes());
            let k = buf.split().freeze();
            buf.put(v.as_bytes());
            let v = buf.split().freeze();
            inner.push((k, v));
        }

        Self { inner }
    }
}

impl Params {
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> Iter<'_, (Bytes, Bytes)> {
        self.inner.iter()
    }

    pub fn get<K: AsRef<[u8]>>(&self, k: K) -> Option<&Bytes> {
        self.iter()
            .filter(|(ik, _)| ik.as_ref() == k.as_ref())
            .map(|(_, v)| v)
            .next()
    }
}
