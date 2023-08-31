use http::{Method, Uri};

use crate::{response::RespBody, HttpContext};
#[async_trait::async_trait]
pub trait FromContext: Sized {
    type Rejection: Into<RespBody>;
    async fn from_context(context: &mut HttpContext) -> Result<Self, Self::Rejection>;
}
#[async_trait::async_trait]
impl<T> FromContext for Option<T>
where
    T: FromContext,
{
    type Rejection = &'static str;

    async fn from_context(context: &mut HttpContext) -> Result<Self, Self::Rejection> {
        Ok(T::from_context(context).await.ok())
    }
}

#[async_trait::async_trait]
impl FromContext for Uri {
    type Rejection = String;

    async fn from_context(context: &mut HttpContext) -> Result<Uri, Self::Rejection> {
        Ok(context.uri.clone())
    }
}

#[async_trait::async_trait]
impl FromContext for Method {
    type Rejection = String;

    async fn from_context(context: &mut HttpContext) -> Result<Method, Self::Rejection> {
        Ok(context.method.clone())
    }
}
