use std::slice::Iter;

use faststr::FastStr;

#[derive(Clone, Debug)]
pub struct Params {
    inner: Vec<(FastStr, FastStr)>,
}

impl Params {
    pub fn new() -> Self {
        Self { inner: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> Iter<'_, (FastStr, FastStr)> {
        self.inner.iter()
    }

    pub fn get<K: AsRef<str>>(&self, k: K) -> Option<FastStr> {
        self.iter()
            .filter(|(ik, _)| ik.as_str() == k.as_ref())
            .map(|(_, v)| v.clone())
            .next()
    }

    pub(crate) fn extend(&mut self, params: matchit::Params<'_, '_>) {
        self.inner.reserve_exact(params.len());

        for (k, v) in params.iter() {
            self.inner.push((k.to_owned().into(), v.to_owned().into()));
        }
    }
}
