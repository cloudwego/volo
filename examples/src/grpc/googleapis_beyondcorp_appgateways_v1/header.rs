use volo::{Layer, Service};
use volo_grpc::{
    context::ClientContext,
    metadata::{KeyAndValueRef, MetadataMap},
    transport::UriExtension,
    Request, Response, Status,
};

use super::endpoint::RpcEndpoint;

#[derive(Clone)]
pub struct RpcHeader {
    endpoint: RpcEndpoint,
    metadata: Option<MetadataMap>,
}

#[derive(Clone)]
pub struct HeaderLayer {
    inner: RpcHeader,
}

impl HeaderLayer {
    pub fn new(endpoint: RpcEndpoint) -> Self {
        Self {
            inner: RpcHeader {
                endpoint,
                metadata: None,
            },
        }
    }

    pub fn metadata(mut self, metadata: Option<MetadataMap>) -> Self {
        self.inner.metadata = metadata;
        self
    }
}

impl<S> Layer<S> for HeaderLayer {
    type Service = HeaderService<S>;

    fn layer(self, inner: S) -> Self::Service {
        HeaderService::new(inner, self.inner)
    }
}

#[derive(Clone)]
pub struct HeaderService<S> {
    inner: S,
    header: RpcHeader,
}

impl<S> HeaderService<S> {
    pub fn new(inner: S, header: RpcHeader) -> Self {
        Self { inner, header }
    }
}

impl<T, U, S> Service<ClientContext, Request<T>> for HeaderService<S>
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
        let extensions = volo_req.extensions_mut();
        extensions.insert(UriExtension(self.header.endpoint.uri()));

        let metadata = volo_req.metadata_mut();

        if let Some(base_meta) = &self.header.metadata {
            for item in base_meta.iter() {
                match item {
                    KeyAndValueRef::Ascii(key, val) => {
                        metadata.insert(key, val.clone());
                    }
                    KeyAndValueRef::Binary(key, val) => {
                        metadata.insert_bin(key, val.clone());
                    }
                }
            }
        }

        let volo_resp = self.inner.call(cx, volo_req).await?;
        Ok(volo_resp)
    }
}
