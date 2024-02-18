use http::{request::Parts, StatusCode};
use volo::context::Context;

use super::Extension;
use crate::{
    context::ServerContext,
    response::ServerResponse,
    server::{extract::FromContext, IntoResponse},
};

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
