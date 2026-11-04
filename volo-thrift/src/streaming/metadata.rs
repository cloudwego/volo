use std::collections::BTreeMap;

use bytes::Bytes;

/// Lossless TTHeader metadata. Values are not assumed to contain UTF-8.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub int: BTreeMap<u16, Bytes>,
    pub string: BTreeMap<Bytes, Bytes>,
}

impl Metadata {
    pub fn get(&self, key: &str) -> Option<&Bytes> {
        self.string.get(key.as_bytes())
    }

    pub fn insert(&mut self, key: impl Into<Bytes>, value: impl Into<Bytes>) -> Option<Bytes> {
        self.string.insert(key.into(), value.into())
    }

    pub fn extend(&mut self, other: Self) {
        self.int.extend(other.int);
        self.string.extend(other.string);
    }
}

/// Export only values written at this hop and persistent values. Upstream transients
/// remain visible to the handler but are intentionally absent from the next request.
pub fn export_metainfo(info: &metainfo::MetaInfo, metadata: &mut Metadata) {
    use metainfo::Forward;
    if let Some(values) = info.get_all_persistents_and_transients_with_rpc_prefix() {
        for (key, value) in values {
            if !key.is_empty() && !value.is_empty() {
                metadata.insert(key.to_string(), value.to_string());
            }
        }
    }
}
pub fn import_metainfo(metadata: &Metadata, info: &mut metainfo::MetaInfo) {
    use metainfo::Forward;
    for (key, value) in &metadata.string {
        let (Ok(key), Ok(value)) = (std::str::from_utf8(key), std::str::from_utf8(value)) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        // TransferForward discards stale/upstream values received on the wire.
        if key.starts_with("RPC_TRANSIT_UPSTREAM_") {
            continue;
        }
        if let Some(key) = key.strip_prefix("RPC_TRANSIT_") {
            if !key.is_empty() {
                info.set_upstream(key.to_owned(), value.to_owned());
            }
        } else if let Some(key) = key.strip_prefix("RPC_PERSIST_") {
            if !key.is_empty() {
                info.set_persistent(key.to_owned(), value.to_owned());
            }
        }
    }
}

/// Takes one outgoing-hop snapshot without turning locally set transients into upstreams.
pub(crate) fn copy_outgoing_metainfo(source: &metainfo::MetaInfo, target: &mut metainfo::MetaInfo) {
    use metainfo::Forward;
    if let Some(values) = source.get_all_transients() {
        for (key, value) in values {
            target.set_transient(key.clone(), value.clone());
        }
    }
    if let Some(values) = source.get_all_persistents() {
        for (key, value) in values {
            target.set_persistent(key.clone(), value.clone());
        }
    }
}
