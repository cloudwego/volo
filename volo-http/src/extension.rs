use std::convert::Infallible;

use hyper::{body::Incoming, StatusCode};
use motore::{layer::Layer, service::Service};

use crate::{extract::FromContext, response::IntoResponse, HttpContext, Response};

#[derive(Debug, Default, Clone, Copy)]
pub struct Extension<T>(pub T);

impl<S, T> Layer<S> for Extension<T>
where
    S: Service<HttpContext, Incoming, Response = Response> + Send + Sync + 'static,
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

impl<S, T> Service<HttpContext, Incoming> for ExtensionService<S, T>
where
    S: Service<HttpContext, Incoming, Response = Response, Error = Infallible>
        + Send
        + Sync
        + 'static,
    T: Clone + Send + Sync + 'static,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut HttpContext,
        req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        cx.extensions_mut().insert(self.ext.clone());
        self.inner.call(cx, req).await
    }
}

impl<T, S> FromContext<S> for Extension<T>
where
    T: Clone + Send + Sync + 'static,
    S: Sync,
{
    type Rejection = ExtensionRejection;

    async fn from_context(cx: &mut HttpContext, _state: &S) -> Result<Self, Self::Rejection> {
        cx.extensions()
            .get::<T>()
            .map(T::clone)
            .map(Extension)
            .ok_or(ExtensionRejection::NotExist)
    }
}

pub enum ExtensionRejection {
    NotExist,
}

impl IntoResponse for ExtensionRejection {
    fn into_response(self) -> Response {
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}
