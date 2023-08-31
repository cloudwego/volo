use http::{Method, Response, Uri};

use crate::{response::IntoResponse, HttpContext};

#[async_trait::async_trait]
pub trait FromContext: Sized {
    type Rejection: IntoResponse;
    async fn from_context(context: &mut HttpContext) -> Result<Self, Self::Rejection>;
}
#[async_trait::async_trait]
impl<T> FromContext for Option<T>
where
    T: FromContext,
{
    type Rejection = Response<()>; // Infallible

    async fn from_context(context: &mut HttpContext) -> Result<Self, Self::Rejection> {
        Ok(T::from_context(context).await.ok())
    }
}

#[async_trait::async_trait]
impl FromContext for Uri {
    type Rejection = Response<()>; // Infallible

    async fn from_context(context: &mut HttpContext) -> Result<Uri, Self::Rejection> {
        Ok(context.uri.clone())
    }
}

#[async_trait::async_trait]
impl FromContext for Method {
    type Rejection = Response<()>;

    async fn from_context(context: &mut HttpContext) -> Result<Method, Self::Rejection> {
        Ok(context.method.clone())
    }
}
