use std::{net::SocketAddr, str::FromStr};

use metainfo::{Backward, Forward};
use volo::{context::Context, Service};

use crate::{
    context::ClientContext,
    metadata::{
        KeyAndValueRef, MetadataKey, DESTINATION_METHOD, DESTINATION_SERVICE,
        HEADER_TRANS_REMOTE_ADDR, SOURCE_SERVICE,
    },
    Request, Response, Status,
};

#[derive(Clone)]
pub struct MetaService<S> {
    inner: S,
}

impl<S> MetaService<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<T, U, S> Service<ClientContext, Request<T>> for MetaService<S>
where
    S: Service<ClientContext, Request<T>, Response = Response<U>, Error = Status>
        + Send
        + 'static
        + Sync,
    T: Send + 'static,
{
    type Response = S::Response;

    type Error = S::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut ClientContext,
        mut volo_req: Request<T>,
    ) -> Result<Self::Response, Self::Error> {
        let metadata = volo_req.metadata_mut();
        _ = metainfo::METAINFO.with(|metainfo| {
            let metainfo = metainfo.borrow_mut();

            // persistents for multi-hops
            if let Some(ap) = metainfo.get_all_persistents() {
                for (key, value) in ap {
                    let key = metainfo::HTTP_PREFIX_PERSISTENT.to_owned() + key;
                    metadata.insert(MetadataKey::from_str(key.as_str())?, value.parse()?);
                }
            }

            // transients for one-hop
            if let Some(at) = metainfo.get_all_transients() {
                for (key, value) in at {
                    let key = metainfo::HTTP_PREFIX_TRANSIENT.to_owned() + key;
                    metadata.insert(MetadataKey::from_str(key.as_str())?, value.parse()?);
                }
            }

            // caller
            metadata.insert(SOURCE_SERVICE, cx.rpc_info.caller().service_name().parse()?);

            // callee
            metadata.insert(
                DESTINATION_SERVICE,
                cx.rpc_info.callee().service_name().parse()?,
            );
            metadata.insert(DESTINATION_METHOD, cx.rpc_info.method().parse()?);

            Ok::<(), Status>(())
        });

        let mut volo_resp = self.inner.call(cx, volo_req).await?;

        let metadata = volo_resp.metadata_mut();
        _ = metainfo::METAINFO.with(|metainfo| {
            let mut metainfo = metainfo.borrow_mut();

            // callee
            if let Some(ad) = metadata.remove(HEADER_TRANS_REMOTE_ADDR) {
                let maybe_addr = ad.to_str()?.parse::<SocketAddr>();
                if let Ok(addr) = maybe_addr {
                    cx.rpc_info_mut()
                        .callee_mut()
                        .set_address(volo::net::Address::from(addr));
                }
            }

            // backward
            let mut vec = Vec::with_capacity(metadata.len());
            for key_and_value in metadata.iter() {
                match key_and_value {
                    KeyAndValueRef::Ascii(k, v) => {
                        let k = k.as_str();
                        let v = v.to_str()?;
                        if k.starts_with(metainfo::HTTP_PREFIX_BACKWARD) {
                            vec.push(k.to_owned());
                            metainfo.strip_http_prefix_and_set_backward_downstream(
                                k.to_owned(),
                                v.to_owned(),
                            );
                        }
                    }
                    _ => unreachable!(),
                }
            }
            for k in vec {
                metadata.remove(k);
            }

            Ok::<(), Status>(())
        });

        Ok(volo_resp)
    }
}
