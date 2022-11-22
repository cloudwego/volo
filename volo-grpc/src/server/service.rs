use std::marker::PhantomData;

use futures::Future;
use motore::{
    layer::{Identity, Layer, Stack},
    service::Service,
};

use super::NamedService;
use crate::{
    body::Body,
    codec::{
        compression::{CompressionEncoding, ENCODING_HEADER},
        decode::Kind,
    },
    context::{Config, ServerContext},
    message::{RecvEntryMessage, SendEntryMessage},
    metadata::MetadataValue,
    Request, Response, Status,
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
        L::Service: Service<ServerContext, Request<T>, Response = Response<U>>,
        <L::Service as Service<ServerContext, Request<T>>>::Error: Into<Status> + Send,
        S: Service<ServerContext, Request<T>, Response = Response<U>, Error = Status>,
        T: RecvEntryMessage,
        U: SendEntryMessage,
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
    _marker: PhantomData<(T, U)>,
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

impl<S, T, U> Service<ServerContext, Request<hyper::Body>> for CodecService<S, T, U>
where
    S: Service<ServerContext, Request<T>, Response = Response<U>> + Clone + Send + Sync + 'static,
    S::Error: Into<Status>,
    T: RecvEntryMessage + Send + Sync,
    U: SendEntryMessage + Send + Sync,
{
    type Response = Response<Body>;
    type Error = Status;
    type Future<'cx> = impl Future<Output = Result<Self::Response, Self::Error>> + Send + 'cx
    where
        Self: 'cx;

    fn call<'cx, 's>(
        &'s self,
        cx: &'cx mut ServerContext,
        req: Request<hyper::Body>,
    ) -> Self::Future<'cx>
    where
        's: 'cx,
    {
        async move {
            let (metadata, extensions, body) = req.into_parts();
            let send_compression = CompressionEncoding::from_accept_encoding_header(
                metadata.headers(),
                &self.rpc_config.send_compressions,
            );

            let recv_compression = CompressionEncoding::from_encoding_header(
                metadata.headers(),
                &self.rpc_config.accept_compressions,
            )?;

            let message = T::from_body(
                cx.rpc_info.method.as_deref(),
                body,
                Kind::Request,
                recv_compression,
            )?;

            let volo_req = Request::from_parts(metadata, extensions, message);

            let volo_resp = self.inner.call(cx, volo_req).await.map_err(Into::into)?;

            let mut resp = volo_resp.map(|message| Body::new(message.into_body(send_compression)));

            if let Some(encoding) = send_compression {
                resp.metadata_mut().insert(
                    ENCODING_HEADER,
                    MetadataValue::unchecked_from_header_value(encoding.into_header_value()),
                );
            };

            Ok(resp)
        }
    }
}

impl<S: NamedService, T, U> NamedService for CodecService<S, T, U> {
    const NAME: &'static str = S::NAME;
}
