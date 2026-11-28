use std::net::SocketAddr;

use metainfo::Backward;
use volo::{Service, context::Context};

use crate::{
    Request, Response, Status,
    context::ClientContext,
    metadata::{
        DESTINATION_METHOD, DESTINATION_SERVICE, HEADER_TRANS_REMOTE_ADDR, KeyAndValueRef,
        SOURCE_SERVICE,
    },
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

    async fn call(
        &self,
        cx: &mut ClientContext,
        mut volo_req: Request<T>,
    ) -> Result<Self::Response, Self::Error> {
        let logical_method = volo_req
            .extensions()
            .get::<crate::message::MethodDescriptor>()
            .filter(|method| method.payload != crate::message::PayloadShape::ProtobufMessage)
            .map(|method| method.name);
        let metadata = volo_req.metadata_mut();
        metainfo::METAINFO.with(|metainfo| {
            let metainfo = metainfo.borrow_mut();

            crate::streaming::metadata::write_forward_fields(&metainfo, metadata.headers_mut())?;

            // caller
            metadata.insert(SOURCE_SERVICE, cx.rpc_info.caller().service_name().parse()?);

            // callee
            metadata.insert(
                DESTINATION_SERVICE,
                cx.rpc_info.callee().service_name().parse()?,
            );
            metadata.insert(
                DESTINATION_METHOD,
                logical_method
                    .unwrap_or_else(|| cx.rpc_info.method().as_str())
                    .parse()?,
            );

            Ok::<(), Status>(())
        })?;

        let mut volo_resp = self.inner.call(cx, volo_req).await?;

        let metadata = volo_resp.metadata_mut();
        metainfo::METAINFO.with(|metainfo| {
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
                        if k.starts_with(metainfo::HTTP_PREFIX_BACKWARD) {
                            let v = std::str::from_utf8(v.as_bytes())
                                .map_err(|_| Status::internal("invalid UTF-8 backward metainfo"))?;
                            vec.push(k.to_owned());
                            metainfo.strip_http_prefix_and_set_backward_downstream(k, v.to_owned());
                        }
                    }
                    KeyAndValueRef::Binary(_, _) => {}
                }
            }
            for k in vec {
                metadata.remove(k);
            }

            Ok::<(), Status>(())
        })?;

        Ok(volo_resp)
    }
}
