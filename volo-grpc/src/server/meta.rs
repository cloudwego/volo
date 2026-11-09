use std::{cell::RefCell, net::SocketAddr, sync::Arc, task::Poll};

use futures::{FutureExt, future::BoxFuture};
use http_body::Body as _;
use metainfo::Forward;
use rustc_hash::FxHashMap;
use tracing::Instrument;
use volo::{FastStr, Service, context::Context};

use super::policy::{HttpRequestInfo, ResponsePolicy, error_response};
use crate::{
    Request, Response, Status,
    body::BoxBody,
    context::{Deadline, ServerContext},
    message::PayloadKind,
    metadata::{DESTINATION_SERVICE, HEADER_TRANS_REMOTE_ADDR, KeyAndValueRef, SOURCE_SERVICE},
    tracing::{DefaultProvider, SpanProvider},
};

macro_rules! status_to_http {
    ($result:expr, $cx:expr) => {
        match $result {
            Ok(value) => value,
            Err(status) => return Ok(error_response(status, $cx)),
        }
    };
}

#[derive(Clone, Debug)]
pub struct MetaService<S, SP = DefaultProvider> {
    inner: S,
    span_provider: SP,
    response_payloads: Option<Arc<FxHashMap<&'static str, PayloadKind>>>,
}

impl<S, SP> MetaService<S, SP> {
    pub fn new(inner: S, span_provider: SP) -> Self {
        MetaService {
            inner,
            span_provider,
            response_payloads: None,
        }
    }

    pub(crate) fn with_response_payloads(
        mut self,
        payloads: Option<Arc<FxHashMap<&'static str, PayloadKind>>>,
    ) -> Self {
        self.response_payloads = payloads;
        self
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

    fn call(&mut self, mut req: hyper::Request<BoxBody>) -> Self::Future {
        let deadline = super::layer::timeout::grpc_timeout_to_duration(req.headers())
            .ok()
            .flatten()
            .and_then(|duration| tokio::time::Instant::now().checked_add(duration))
            .map(Deadline::new);
        let inner = self.inner.clone();
        let span_provider = self.span_provider.clone();
        // Select only the error response format from the registered service.
        // Actual method/content-type validation stays after routing.
        let payload = self.response_payloads.as_ref().and_then(|payloads| {
            let service = req.uri().path().strip_prefix('/')?.split_once('/')?.0;
            payloads.get(service).copied()
        });
        if payload == Some(PayloadKind::Thrift) {
            let info = HttpRequestInfo {
                method: req.method().clone(),
                version: req.version(),
            };
            req.extensions_mut().insert(info);
        }
        async move {
            let mut cx = ServerContext::default();
            if let Some(payload) = payload {
                cx.extensions.insert(payload);
                req.extensions_mut().insert(payload);
                cx.extensions.insert(ResponsePolicy {
                    payload,
                    http_status: None,
                });
            }

            metainfo::METAINFO
                .scope(RefCell::new(metainfo::MetaInfo::default()), async move {
                    cx.rpc_info.set_method(FastStr::new(req.uri().path()));

                    let mut volo_req = Request::from_http(req);
                    if let Some(deadline) = deadline {
                        volo_req.extensions_mut().insert(deadline);
                        metainfo::METAINFO.with(|info| info.borrow_mut().insert(deadline));
                    }

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
                                    if k.starts_with(metainfo::HTTP_PREFIX_PERSISTENT) {
                                        let v =
                                            std::str::from_utf8(v.as_bytes()).map_err(|_| {
                                                Status::invalid_argument(
                                                    "invalid UTF-8 persistent metainfo",
                                                )
                                            })?;
                                        vec.push(k.to_owned());
                                        metainfo
                                            .strip_http_prefix_and_set_persistent(k, v.to_owned());
                                    } else if k.starts_with(metainfo::HTTP_PREFIX_TRANSIENT) {
                                        let v =
                                            std::str::from_utf8(v.as_bytes()).map_err(|_| {
                                                Status::invalid_argument(
                                                    "invalid UTF-8 transient metainfo",
                                                )
                                            })?;
                                        vec.push(k.to_owned());
                                        metainfo
                                            .strip_http_prefix_and_set_upstream(k, v.to_owned());
                                    }
                                }
                                KeyAndValueRef::Binary(_, _) => {}
                            }
                        }
                        for k in vec {
                            metadata.remove(k);
                        }

                        Ok::<(), Status>(())
                    });
                    status_to_http!(status, &cx);
                    let span = span_provider.on_serve(&cx, metadata);
                    let volo_resp = match inner.call(&mut cx, volo_req).instrument(span).await {
                        Ok(resp) => resp,
                        Err(err) => {
                            span_provider.leave_serve(&cx);
                            return Ok(error_response(err.into(), &cx));
                        }
                    };
                    span_provider.leave_serve(&cx);

                    let (mut metadata, extensions, message) = volo_resp.into_parts();

                    let mut write_backward = |info: &metainfo::MetaInfo| {
                        crate::streaming::metadata::write_backward_fields(
                            info,
                            metadata.headers_mut(),
                        )
                    };
                    let status = if let Some(stream) =
                        cx.extensions.get::<crate::streaming::StreamContext>()
                    {
                        stream.with_metainfo(write_backward)
                    } else {
                        metainfo::METAINFO.with(|metainfo| write_backward(&metainfo.borrow()))
                    };
                    status_to_http!(status, &cx);

                    if let Some(stream) = cx.extensions.get::<crate::streaming::StreamContext>() {
                        let mut defaults = stream.response_metadata(false);
                        defaults.merge(metadata);
                        metadata = defaults;
                        stream.on_header(metadata.clone());
                        if message.is_end_stream() {
                            if let Some(status) = Status::from_header_map(metadata.headers()) {
                                stream.on_trailer(metadata.clone());
                                stream.finish(status);
                            }
                        }
                    }

                    let mut resp = hyper::Response::new(message);
                    *resp.headers_mut() = metadata.into_headers();
                    *resp.extensions_mut() = extensions;
                    cx.extensions
                        .get::<ResponsePolicy>()
                        .copied()
                        .unwrap_or_default()
                        .apply(&mut resp);
                    Ok(resp)
                })
                .await
        }
        .boxed()
    }
}
