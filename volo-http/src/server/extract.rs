use std::{convert::Infallible, marker::PhantomData};

use bytes::Bytes;
use faststr::FastStr;
use futures_util::Future;
use http::{header, request::Parts, Method, Uri};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use volo::{context::Context, net::Address};

use super::IntoResponse;
use crate::{
    context::ServerContext,
    error::server::{body_collection_error, ExtractBodyError},
    request::ServerRequest,
};

mod private {
    #[derive(Debug, Clone, Copy)]
    pub enum ViaContext {}

    #[derive(Debug, Clone, Copy)]
    pub enum ViaRequest {}
}

pub trait FromContext: Sized {
    type Rejection: IntoResponse;

    fn from_context(
        cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send;
}

pub trait FromRequest<M = private::ViaRequest>: Sized {
    type Rejection: IntoResponse;

    fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: Incoming,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Query<T>(pub T);

#[derive(Debug, Default, Clone, Copy)]
pub struct Form<T>(pub T);

#[derive(Debug, Default, Clone)]
pub struct MaybeInvalid<T>(Vec<u8>, PhantomData<T>);

impl MaybeInvalid<String> {
    /// # Safety
    ///
    /// It is up to the caller to guarantee that the value really is valid. Using this when the
    /// content is invalid causes immediate undefined behavior.
    pub unsafe fn assume_valid(self) -> String {
        String::from_utf8_unchecked(self.0)
    }
}

impl MaybeInvalid<FastStr> {
    /// # Safety
    ///
    /// It is up to the caller to guarantee that the value really is valid. Using this when the
    /// content is invalid causes immediate undefined behavior.
    pub unsafe fn assume_valid(self) -> FastStr {
        FastStr::from_vec_u8_unchecked(self.0)
    }
}

impl<T> FromContext for Option<T>
where
    T: FromContext,
{
    type Rejection = Infallible;

    async fn from_context(
        cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> Result<Self, Self::Rejection> {
        Ok(T::from_context(cx, parts).await.ok())
    }
}

impl FromContext for Address {
    type Rejection = Infallible;

    async fn from_context(
        cx: &mut ServerContext,
        _parts: &mut Parts,
    ) -> Result<Address, Self::Rejection> {
        Ok(cx
            .rpc_info()
            .caller()
            .address()
            .expect("server context does not have caller address"))
    }
}

impl FromContext for Uri {
    type Rejection = Infallible;

    async fn from_context(
        _cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> Result<Uri, Self::Rejection> {
        Ok(parts.uri.to_owned())
    }
}

impl FromContext for Method {
    type Rejection = Infallible;

    async fn from_context(
        _cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> Result<Method, Self::Rejection> {
        Ok(parts.method.to_owned())
    }
}

#[cfg(feature = "query")]
#[cfg_attr(docsrs, doc(cfg(feature = "query")))]
impl<T> FromContext for Query<T>
where
    T: serde::de::DeserializeOwned,
{
    type Rejection = serde_urlencoded::de::Error;

    async fn from_context(
        _cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> Result<Self, Self::Rejection> {
        let query = parts.uri.query().unwrap_or_default();
        let param = serde_urlencoded::from_str(query)?;
        Ok(Query(param))
    }
}

#[cfg(feature = "query")]
#[cfg_attr(docsrs, doc(cfg(feature = "query")))]
impl IntoResponse for serde_urlencoded::de::Error {
    fn into_response(self) -> crate::response::ServerResponse {
        http::StatusCode::BAD_REQUEST.into_response()
    }
}

impl<T> FromRequest<private::ViaContext> for T
where
    T: FromContext + Sync,
{
    type Rejection = T::Rejection;

    async fn from_request(
        cx: &mut ServerContext,
        mut parts: Parts,
        _body: Incoming,
    ) -> Result<Self, Self::Rejection> {
        T::from_context(cx, &mut parts).await
    }
}

impl<T> FromRequest for Option<T>
where
    T: FromRequest<private::ViaRequest> + Sync,
{
    type Rejection = Infallible;

    async fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: Incoming,
    ) -> Result<Self, Self::Rejection> {
        Ok(T::from_request(cx, parts, body).await.ok())
    }
}

impl FromRequest for ServerRequest {
    type Rejection = Infallible;

    async fn from_request(
        _cx: &mut ServerContext,
        parts: Parts,
        body: Incoming,
    ) -> Result<Self, Self::Rejection> {
        Ok(ServerRequest::from_parts(parts, body))
    }
}

impl FromRequest for Vec<u8> {
    type Rejection = ExtractBodyError;

    async fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: Incoming,
    ) -> Result<Self, Self::Rejection> {
        Ok(Bytes::from_request(cx, parts, body).await?.into())
    }
}

impl FromRequest for Bytes {
    type Rejection = ExtractBodyError;

    async fn from_request(
        _cx: &mut ServerContext,
        parts: Parts,
        body: Incoming,
    ) -> Result<Self, Self::Rejection> {
        let bytes = body
            .collect()
            .await
            .map_err(|_| body_collection_error())?
            .to_bytes();

        if let Some(Ok(Ok(cap))) = parts
            .headers
            .get(header::CONTENT_LENGTH)
            .map(|v| v.to_str().map(|c| c.parse::<usize>()))
        {
            if bytes.len() != cap {
                tracing::warn!(
                    "The length of body ({}) does not match the Content-Length ({})",
                    bytes.len(),
                    cap
                );
            }
        }

        Ok(bytes)
    }
}

impl FromRequest for String {
    type Rejection = ExtractBodyError;

    async fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: Incoming,
    ) -> Result<Self, Self::Rejection> {
        let vec = Vec::<u8>::from_request(cx, parts, body).await?;

        // Check if the &[u8] is a valid string
        let _ = simdutf8::basic::from_utf8(&vec).map_err(ExtractBodyError::String)?;

        // SAFETY: The `Vec<u8>` is checked by `simdutf8` and it is a valid `String`
        Ok(unsafe { String::from_utf8_unchecked(vec) })
    }
}

impl FromRequest for FastStr {
    type Rejection = ExtractBodyError;

    async fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: Incoming,
    ) -> Result<Self, Self::Rejection> {
        let vec = Vec::<u8>::from_request(cx, parts, body).await?;

        // Check if the &[u8] is a valid string
        let _ = simdutf8::basic::from_utf8(&vec).map_err(ExtractBodyError::String)?;

        // SAFETY: The `Vec<u8>` is checked by `simdutf8` and it is a valid `String`
        Ok(unsafe { FastStr::from_vec_u8_unchecked(vec) })
    }
}

impl<T> FromRequest for MaybeInvalid<T> {
    type Rejection = ExtractBodyError;

    async fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: Incoming,
    ) -> Result<Self, Self::Rejection> {
        let vec = Vec::<u8>::from_request(cx, parts, body).await?;

        Ok(MaybeInvalid(vec, PhantomData))
    }
}

#[cfg(feature = "form")]
#[cfg_attr(docsrs, doc(cfg(feature = "form")))]
impl<T> FromRequest for Form<T>
where
    T: serde::de::DeserializeOwned,
{
    type Rejection = ExtractBodyError;

    async fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: Incoming,
    ) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(cx, parts, body).await?;
        let form =
            serde_urlencoded::from_bytes::<T>(bytes.as_ref()).map_err(ExtractBodyError::Form)?;

        Ok(Form(form))
    }
}
