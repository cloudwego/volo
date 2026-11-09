use std::marker::PhantomData;

use motore::{
    layer::{Identity, Layer, Stack},
    service::Service,
};

use super::{
    NamedService,
    policy::{HttpRequestInfo, ResponsePolicy, validate_content_type, validate_request_info},
};
use crate::{
    Request, Response, Status,
    body::{Body, BoxBody, boxed},
    codec::{
        compression::{CompressionEncoding, ENCODING_HEADER},
        decode::Kind,
    },
    context::{Config, ServerContext},
    message::{CodecOptions, RecvEntryMessage, SendEntryMessage},
    metadata::MetadataValue,
};

#[derive(Clone)]
pub struct ServiceBuilder<S, L> {
    service: S,
    layer: L,
    rpc_config: Config,
}

impl<S> ServiceBuilder<S, Identity> {
    pub fn new(service: S) -> Self {
        Self {
            service,
            layer: Identity::new(),
            rpc_config: Config::default(),
        }
    }
}

impl<S, L> ServiceBuilder<S, L> {
    pub fn stream_hook(mut self, factory: impl crate::streaming::StreamHookFactory) -> Self {
        let next = crate::streaming::HookFactory(std::sync::Arc::new(factory));
        self.rpc_config.stream_hook = Some(match self.rpc_config.stream_hook.take() {
            Some(previous) => previous.append(next),
            None => next,
        });
        self
    }

    pub fn codec_limits(mut self, limits: crate::codec::CodecLimits) -> Self {
        self.rpc_config.codec_limits = Some(limits);
        self
    }
    /// Sets the send compression encodings for the request, and will self-adaptive with config of
    /// the client.
    ///
    /// Default is disable the send compression.
    pub fn send_compressions(mut self, config: Vec<CompressionEncoding>) -> Self {
        self.rpc_config.send_compressions = Some(config);
        self
    }

    /// Sets the accept compression encodings for the request, and will self-adaptive with config of
    /// the server.
    ///
    /// Default is disable the accept decompression.
    pub fn accept_compressions(mut self, config: Vec<CompressionEncoding>) -> Self {
        self.rpc_config.accept_compressions = Some(config);
        self
    }

    pub fn layer<O>(self, layer: O) -> ServiceBuilder<S, Stack<O, L>> {
        ServiceBuilder {
            layer: Stack::new(layer, self.layer),
            service: self.service,
            rpc_config: self.rpc_config,
        }
    }

    pub fn layer_front<Front>(self, layer: Front) -> ServiceBuilder<S, Stack<L, Front>> {
        ServiceBuilder {
            layer: Stack::new(self.layer, layer),
            service: self.service,
            rpc_config: self.rpc_config,
        }
    }

    pub fn build<T, U>(self) -> CodecService<<L as volo::Layer<S>>::Service, T, U>
    where
        L: Layer<S>,
    {
        let service = motore::builder::ServiceBuilder::new()
            .layer(self.layer)
            .service(self.service);

        CodecService::new(service, self.rpc_config)
    }
}

pub struct CodecService<S, T, U> {
    inner: S,
    rpc_config: Config,
    _marker: PhantomData<fn(T, U)>,
}

impl<S, T, U> Clone for CodecService<S, T, U>
where
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            rpc_config: self.rpc_config.clone(),
            _marker: PhantomData,
        }
    }
}

impl<S, T, U> CodecService<S, T, U> {
    pub fn new(inner: S, rpc_config: Config) -> Self {
        Self {
            inner,
            rpc_config,
            _marker: PhantomData,
        }
    }
}

impl<S, T, U> Service<ServerContext, Request<BoxBody>> for CodecService<S, T, U>
where
    S: Service<ServerContext, Request<T>, Response = Response<U>> + Sync,
    S::Error: Into<Status>,
    T: RecvEntryMessage,
    U: SendEntryMessage,
{
    type Response = Response<BoxBody>;
    type Error = Status;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: Request<BoxBody>,
    ) -> Result<Self::Response, Self::Error> {
        let (metadata, extensions, mut body) = req.into_parts();
        if let Some(stream) = extensions.get::<crate::streaming::StreamContext>() {
            body = crate::streaming::lifecycle_body::request(body, stream.clone());
        }
        let payload = T::payload_kind();
        if payload == crate::message::PayloadKind::Thrift
            || cx.extensions.get::<ResponsePolicy>().is_some()
        {
            cx.extensions.insert(ResponsePolicy {
                payload,
                http_status: None,
            });
        }
        validate_content_type(metadata.headers(), payload, cx)?;
        validate_request_info(extensions.get::<HttpRequestInfo>(), payload, cx)?;
        let options = CodecOptions::new(
            self.rpc_config
                .codec_limits
                .unwrap_or_else(|| payload.default_limits()),
        )
        .with_context(extensions.get::<crate::streaming::StreamContext>().cloned());
        #[cfg(not(feature = "compress"))]
        let send_compression = None;
        #[cfg(feature = "compress")]
        let send_compression = CompressionEncoding::from_accept_encoding_header(
            metadata.headers(),
            &self.rpc_config.send_compressions,
        );

        let recv_compression = if payload == crate::message::PayloadKind::Thrift {
            CompressionEncoding::from_encoding_header_strict(
                metadata.headers(),
                &self.rpc_config.accept_compressions,
            )?
        } else {
            #[cfg(feature = "compress")]
            {
                CompressionEncoding::from_encoding_header(
                    metadata.headers(),
                    &self.rpc_config.accept_compressions,
                )?
            }
            #[cfg(not(feature = "compress"))]
            {
                None
            }
        };

        let message = T::from_body_with_options(
            Some(cx.rpc_info.method().as_str()),
            body,
            Kind::Request,
            recv_compression,
            options.clone(),
        )?;

        let volo_req = Request::from_parts(metadata, extensions, message);

        cx.stats.record_process_start_at();

        let volo_resp = self.inner.call(cx, volo_req).await.map_err(|error| {
            let status: Status = error.into();
            if payload == crate::message::PayloadKind::Thrift && status.code() == crate::Code::Ok {
                Status::internal("Thrift handler returned OK as an error")
            } else {
                status
            }
        })?;

        cx.stats.record_process_end_at();

        let mut resp = volo_resp.map(|message| {
            boxed(Body::new(
                message.into_body_with_options(send_compression, options),
            ))
        });

        if let Some(encoding) = send_compression {
            resp.metadata_mut().insert(
                ENCODING_HEADER,
                MetadataValue::unchecked_from_header_value(encoding.into_header_value()),
            );
        };

        Ok(resp)
    }
}

impl<S: NamedService, T, U> NamedService for CodecService<S, T, U> {
    fn stream_hook_factory(
        &self,
    ) -> Option<std::sync::Arc<dyn crate::streaming::StreamHookFactory>> {
        self.rpc_config
            .stream_hook
            .as_ref()
            .map(|factory| factory.0.clone())
    }
    const NAME: &'static str = S::NAME;

    fn descriptor() -> Option<crate::message::ServiceDescriptor> {
        S::descriptor()
    }
}
