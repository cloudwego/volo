use std::{cell::RefCell, net::SocketAddr, str::FromStr, sync::Arc, task::Poll};

use futures::{FutureExt, future::BoxFuture};
use metainfo::{Backward, Forward};
use volo::{FastStr, Service, context::Context};

use crate::{
    Request, Response, Status,
    body::BoxBody,
    context::ServerContext,
    metadata::{
        DESTINATION_SERVICE, HEADER_TRANS_REMOTE_ADDR, KeyAndValueRef, MetadataKey, SOURCE_SERVICE,
    },
    tracing::SpanProvider,
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
pub struct MetaService<S, SP> {
    inner: S,
    span_provider: SP,
}

impl<S, SP> MetaService<S, SP> {
    pub fn new(inner: S, span_provider: SP) -> Self {
        MetaService {
            inner,
            span_provider,
        }
    }
}

impl<S, SP> tower::Service<hyper::Request<BoxBody>> for MetaService<S, SP>
where
    S: Service<ServerContext, Request<BoxBody>, Response = Response<BoxBody>>
        + Clone
        + Send
        + Sync
        + 'static,
    S::Error: Into<Status>,
    SP: SpanProvider,
{
    type Response = hyper::Response<BoxBody>;

    type Error = Status;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: hyper::Request<BoxBody>) -> Self::Future {
        let inner = self.inner.clone();
        let span_provider = self.span_provider.clone();
        async move {
            let mut cx = ServerContext::default();

            metainfo::METAINFO
                .scope(RefCell::new(metainfo::MetaInfo::default()), async move {
                    cx.rpc_info.set_method(FastStr::new(req.uri().path()));

                    let mut volo_req = Request::from_http(req);

                    let metadata = volo_req.metadata_mut();

                    let status = metainfo::METAINFO.with(|metainfo| {
                        let mut metainfo = metainfo.borrow_mut();

                        // caller
                        if let Some(source_service) = metadata.remove(SOURCE_SERVICE) {
                            let source_service = Arc::<str>::from(source_service.to_str()?);
                            let caller = cx.rpc_info_mut().caller_mut();
                            caller.set_service_name(source_service.into());
                            if let Some(ad) = metadata.remove(HEADER_TRANS_REMOTE_ADDR) {
                                let addr = ad.to_str()?.parse::<SocketAddr>();
                                if let Ok(addr) = addr {
                                    caller.set_address(volo::net::Address::from(addr));
                                }
                            }
                        }

                        // callee
                        if let Some(destination_service) = metadata.remove(DESTINATION_SERVICE) {
                            let destination_service =
                                Arc::<str>::from(destination_service.to_str()?);
                            cx.rpc_info_mut()
                                .callee_mut()
                                .set_service_name(destination_service.into());
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
                                        metainfo
                                            .strip_http_prefix_and_set_persistent(k, v.to_owned());
                                    } else if k.starts_with(metainfo::HTTP_PREFIX_TRANSIENT) {
                                        vec.push(k.to_owned());
                                        metainfo
                                            .strip_http_prefix_and_set_upstream(k, v.to_owned());
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
                    status_to_http!(status);

                    let span = span_provider.on_serve(&cx);
                    let _enter = span.enter();
                    let volo_resp = match inner.call(&mut cx, volo_req).await {
                        Ok(resp) => resp,
                        Err(err) => {
                            return Ok(err.into().to_http());
                        }
                    };
                    span_provider.leave_serve(&cx);

                    let (mut metadata, extensions, message) = volo_resp.into_parts();

                    let status = metainfo::METAINFO.with(|metainfo| {
                        let metainfo = metainfo.borrow_mut();

                        // backward
                        if let Some(at) = metainfo.get_all_backward_transients() {
                            for (key, value) in at {
                                let key = metainfo::HTTP_PREFIX_BACKWARD.to_owned() + key;
                                metadata
                                    .insert(MetadataKey::from_str(key.as_str())?, value.parse()?);
                            }
                        }

                        Ok::<(), Status>(())
                    });
                    status_to_http!(status);

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
        .boxed()
    }
}
