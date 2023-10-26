use std::{cell::RefCell, net::SocketAddr, str::FromStr, sync::Arc};

use metainfo::{Backward, Forward};
use volo::{
    context::{Context, Endpoint},
    net::Address,
    FastStr, Service,
};

use crate::{
    body::Body,
    context::ServerContext,
    metadata::{
        KeyAndValueRef, MetadataKey, DESTINATION_SERVICE, HEADER_TRANS_REMOTE_ADDR, SOURCE_SERVICE,
    },
    Request, Response, Status,
};

macro_rules! status_to_http {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(status) => return Ok(status.to_http()),
        }
    };
}

#[derive(Clone, Debug)]
pub struct MetaService<S> {
    inner: S,
    peer_addr: Option<Address>,
}

impl<S> MetaService<S> {
    pub fn new(inner: S, peer_addr: Option<Address>) -> Self {
        MetaService { inner, peer_addr }
    }
}

impl<S> Service<ServerContext, hyper::Request<hyper::Body>> for MetaService<S>
where
    S: Service<ServerContext, Request<hyper::Body>, Response = Response<Body>>
        + Clone
        + Send
        + Sync
        + 'static,
    S::Error: Into<Status>,
{
    type Response = hyper::Response<Body>;

    type Error = Status;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut ServerContext,
        req: hyper::Request<hyper::Body>,
    ) -> Result<Self::Response, Self::Error> {
        let peer_addr = self.peer_addr.clone();

        metainfo::METAINFO
            .scope(RefCell::new(metainfo::MetaInfo::default()), async move {
                cx.rpc_info.method = Some(FastStr::new(req.uri().path()));

                let mut volo_req = Request::from_http(req);

                let metadata = volo_req.metadata_mut();

                status_to_http!(metainfo::METAINFO.with(|metainfo| {
                    let mut metainfo = metainfo.borrow_mut();

                    // caller
                    if let Some(source_service) = metadata.remove(SOURCE_SERVICE) {
                        let source_service = Arc::<str>::from(source_service.to_str()?);
                        let mut caller = Endpoint::new(source_service.into());
                        if let Some(ad) = metadata.remove(HEADER_TRANS_REMOTE_ADDR) {
                            let addr = ad.to_str()?.parse::<SocketAddr>();
                            if let Ok(addr) = addr {
                                caller.set_address(volo::net::Address::from(addr));
                            }
                        }
                        if caller.address.is_none() {
                            caller.address = peer_addr;
                        }
                        cx.rpc_info_mut().caller = Some(caller);
                    }

                    // callee
                    if let Some(destination_service) = metadata.remove(DESTINATION_SERVICE) {
                        let destination_service = Arc::<str>::from(destination_service.to_str()?);
                        let callee = Endpoint::new(destination_service.into());
                        cx.rpc_info_mut().callee = Some(callee);
                    }

                    // persistent and transient
                    let mut vec = Vec::with_capacity(metadata.len());
                    for key_and_value in metadata.iter() {
                        match key_and_value {
                            KeyAndValueRef::Ascii(k, v) => {
                                let k = k.as_str();
                                let v = v.to_str()?;
                                if k.starts_with(metainfo::HTTP_PREFIX_PERSISTENT) {
                                    vec.push(k.to_owned());
                                    metainfo.strip_http_prefix_and_set_persistent(
                                        k.to_owned(),
                                        v.to_owned(),
                                    );
                                } else if k.starts_with(metainfo::HTTP_PREFIX_TRANSIENT) {
                                    vec.push(k.to_owned());
                                    metainfo.strip_http_prefix_and_set_upstream(
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
                }));

                let volo_resp = match self.inner.call(cx, volo_req).await {
                    Ok(resp) => resp,
                    Err(err) => {
                        return Ok(err.into().to_http());
                    }
                };

                let (mut metadata, extensions, message) = volo_resp.into_parts();

                status_to_http!(metainfo::METAINFO.with(|metainfo| {
                    let metainfo = metainfo.borrow_mut();

                    // backward
                    if let Some(at) = metainfo.get_all_backward_transients() {
                        for (key, value) in at {
                            let key = metainfo::HTTP_PREFIX_BACKWARD.to_owned() + key;
                            metadata.insert(MetadataKey::from_str(key.as_str())?, value.parse()?);
                        }
                    }

                    Ok::<(), Status>(())
                }));

                let mut resp = hyper::Response::new(message);
                *resp.headers_mut() = metadata.into_headers();
                *resp.extensions_mut() = extensions;
                resp.headers_mut().insert(
                    http::header::CONTENT_TYPE,
                    http::header::HeaderValue::from_static("application/grpc"),
                );

                Ok(resp)
            })
            .await
    }
}
