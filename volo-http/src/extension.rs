use std::convert::Infallible;

use http::{request::Parts, StatusCode};
use motore::{layer::Layer, service::Service};
use volo::context::Context;

use crate::{
    context::ServerContext,
    extract::FromContext,
    request::ServerRequest,
    response::{IntoResponse, ServerResponse},
};

#[derive(Debug, Default, Clone, Copy)]
pub struct Extension<T>(pub T);

impl<S, T> Layer<S> for Extension<T>
where
    S: Service<ServerContext, ServerRequest, Response = ServerResponse> + Send + Sync + 'static,
    T: Sync,
{
    type Service = ExtensionService<S, T>;

    fn layer(self, inner: S) -> Self::Service {
        ExtensionService { inner, ext: self.0 }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ExtensionService<I, T> {
    inner: I,
    ext: T,
}

impl<S, T> Service<ServerContext, ServerRequest> for ExtensionService<S, T>
where
    S: Service<ServerContext, ServerRequest, Response = ServerResponse, Error = Infallible>
        + Send
        + Sync
        + 'static,
    T: Clone + Send + Sync + 'static,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut ServerContext,
        req: ServerRequest,
    ) -> Result<Self::Response, Self::Error> {
        cx.extensions_mut().insert(self.ext.clone());
        self.inner.call(cx, req).await
    }
}

impl<T> FromContext for Extension<T>
where
    T: Clone + Send + Sync + 'static,
{
    type Rejection = ExtensionRejection;

    async fn from_context(
        cx: &mut ServerContext,
        _parts: &mut Parts,
    ) -> Result<Self, Self::Rejection> {
        cx.extensions()
            .get::<T>()
            .cloned()
            .map(Extension)
            .ok_or(ExtensionRejection::NotExist)
    }
}

pub enum ExtensionRejection {
    NotExist,
}

impl IntoResponse for ExtensionRejection {
    fn into_response(self) -> ServerResponse {
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}
