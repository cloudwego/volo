use futures_util::Future;
use http::{Method, Response, Uri};

use crate::{response::IntoResponse, HttpContext, Params};

pub trait FromContext: Sized {
    type Rejection: IntoResponse;
    fn from_context(
        context: &HttpContext,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send;
}

impl<T> FromContext for Option<T>
where
    T: FromContext,
{
    type Rejection = Response<()>; // Infallible

    fn from_context(
        context: &HttpContext,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        async move { Ok(T::from_context(context).await.ok()) }
    }
}

impl FromContext for Uri {
    type Rejection = Response<()>; // Infallible

    fn from_context(
        context: &HttpContext,
    ) -> impl Future<Output = Result<Uri, Self::Rejection>> + Send {
        async move { Ok(context.uri.clone()) }
    }
}

impl FromContext for Method {
    type Rejection = Response<()>; // Infallible

    async fn from_context(context: &HttpContext) -> Result<Method, Self::Rejection> {
        Ok(context.method.clone())
    }
}

impl FromContext for Params {
    type Rejection = Response<()>; // Infallible

    fn from_context(
        context: &HttpContext,
    ) -> impl Future<Output = Result<Params, Self::Rejection>> + Send {
        async move { Ok(context.params.clone()) }
    }
}
