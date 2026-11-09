use metainfo::{Backward, Forward};

use super::StreamContext;
use crate::{
    Status,
    metadata::{KeyAndValueRef, MetadataMap},
};

pub(super) fn read_backward(cx: &StreamContext, metadata: &MetadataMap) -> Result<(), Status> {
    cx.update_metainfo(|metainfo| {
        for entry in metadata.iter() {
            let KeyAndValueRef::Ascii(key, value) = entry else {
                continue;
            };
            let key = key.as_str();
            if key.starts_with(metainfo::HTTP_PREFIX_BACKWARD) {
                let value = std::str::from_utf8(value.as_bytes())
                    .map_err(|_| Status::internal("invalid UTF-8 backward metainfo"))?;
                metainfo.strip_http_prefix_and_set_backward_downstream(key, value.to_owned());
            }
        }
        Ok(())
    })
}

pub(super) fn write_backward(
    cx: &StreamContext,
    trailer: &mut http::HeaderMap,
) -> Result<(), Status> {
    cx.with_metainfo(|metainfo| write_backward_fields(metainfo, trailer))
}

/// Export only current-hop transient values and persistent values. Do not use
/// metainfo 0.7.14's HTTP export converter: its heap branch writes the key over
/// the prefix and leaves the suffix uninitialized. Upstream values are never
/// included, independently of a peer's historical propagation flags.
pub(crate) fn write_forward_fields(
    info: &metainfo::MetaInfo,
    headers: &mut http::HeaderMap,
) -> Result<(), Status> {
    for (prefix, values) in [
        (metainfo::HTTP_PREFIX_PERSISTENT, info.get_all_persistents()),
        (metainfo::HTTP_PREFIX_TRANSIENT, info.get_all_transients()),
    ] {
        if let Some(values) = values {
            for (key, value) in values {
                let key = format!("{prefix}{}", key.to_ascii_lowercase().replace('_', "-"))
                    .parse::<http::HeaderName>()
                    .map_err(|_| Status::invalid_argument("invalid metainfo key"))?;
                if key.as_str().ends_with("-bin") {
                    return Err(Status::invalid_argument(
                        "metainfo keys must not end in reserved -bin suffix",
                    ));
                }
                let value = http::HeaderValue::from_bytes(value.as_bytes())
                    .map_err(|_| Status::invalid_argument("invalid metainfo value"))?;
                headers.insert(key, value);
            }
        }
    }
    Ok(())
}

// metainfo 0.7.14's backward export helpers use the transient prefix and its
// converter is private. Preserve its HTTP key normalization with the correct
// backward prefix, without forking or modifying the dependency.
pub(crate) fn write_backward_fields(
    info: &metainfo::MetaInfo,
    headers: &mut http::HeaderMap,
) -> Result<(), Status> {
    if let Some(values) = info.get_all_backward_transients() {
        for (key, value) in values {
            let key = format!(
                "{}{}",
                metainfo::HTTP_PREFIX_BACKWARD,
                key.to_ascii_lowercase().replace('_', "-")
            )
            .parse::<http::HeaderName>()
            .map_err(|_| Status::internal("invalid backward metainfo key"))?;
            let value = http::HeaderValue::from_bytes(value.as_bytes())
                .map_err(|_| Status::internal("invalid backward metainfo value"))?;
            headers.insert(key, value);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use volo::context::Role;

    use super::*;

    #[test]
    fn long_forward_keys_keep_their_prefix_and_upstream_is_single_hop() {
        let mut info = metainfo::MetaInfo::default();
        info.set_transient("INTEROP_ONCE_WITH_A_LONG_KEY", "one-hop");
        info.set_persistent("INTEROP_PERSIST_WITH_A_LONG_KEY", "many-hops");
        info.set_upstream("PREVIOUS_HOP", "must-not-leak");
        let mut headers = http::HeaderMap::new();
        write_forward_fields(&info, &mut headers).unwrap();
        assert_eq!(
            headers["rpc-transit-interop-once-with-a-long-key"],
            "one-hop"
        );
        assert_eq!(
            headers["rpc-persist-interop-persist-with-a-long-key"],
            "many-hops"
        );
        assert!(!headers.contains_key("rpc-transit-previous-hop"));
        assert_eq!(headers.len(), 2);
    }

    #[tokio::test]
    async fn backward_trailer_overrides_header_before_finish_and_ignores_binary() {
        let cx = StreamContext::new("/test/Call", None, Role::Client, None);
        let key = format!("{}location", metainfo::HTTP_PREFIX_BACKWARD);
        let mut header = MetadataMap::new();
        header.insert(
            key.parse::<crate::metadata::MetadataKey<crate::metadata::Ascii>>()
                .unwrap(),
            "old".parse().unwrap(),
        );
        header.insert_bin(
            "opaque-bin",
            crate::metadata::MetadataValue::from_bytes(b"\xff"),
        );
        read_backward(&cx, &header).unwrap();
        let mut trailer = MetadataMap::new();
        trailer.insert(
            key.parse::<crate::metadata::MetadataKey<crate::metadata::Ascii>>()
                .unwrap(),
            crate::metadata::MetadataValue::try_from_bytes("新值".as_bytes()).unwrap(),
        );
        read_backward(&cx, &trailer).unwrap();
        cx.with_metainfo(|mi| {
            assert_eq!(
                mi.get_backward_downstream("LOCATION").as_deref(),
                Some("新值")
            )
        });
    }
}
