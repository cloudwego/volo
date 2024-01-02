use std::{convert::Infallible, marker::PhantomData};

use bytes::Bytes;
use faststr::FastStr;
use futures_util::Future;
use http_body_util::BodyExt;
use hyper::{
    body::Incoming,
    http::{header, HeaderMap, Method, StatusCode, Uri},
};
use serde::de::DeserializeOwned;
use volo::net::Address;

use crate::{
    context::{ConnectionInfo, HttpContext},
    param::Params,
    response::IntoResponse,
};

mod private {
    #[derive(Debug, Clone, Copy)]
    pub enum ViaContext {}

    #[derive(Debug, Clone, Copy)]
    pub enum ViaRequest {}
}

pub trait FromContext<S>: Sized {
    type Rejection: IntoResponse;

    fn from_context(
        context: &HttpContext,
        state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send;
}

pub trait FromRequest<S, M = private::ViaRequest>: Sized {
    type Rejection: IntoResponse;

    fn from_request(
        cx: &HttpContext,
        body: Incoming,
        state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct State<S>(pub S);

#[derive(Debug, Default, Clone, Copy)]
pub struct Query<T>(pub T);

#[derive(Debug, Default, Clone, Copy)]
pub struct Form<T>(pub T);

#[derive(Debug, Default, Clone)]
pub struct MaybeInvalid<T>(Vec<u8>, PhantomData<T>);

impl MaybeInvalid<String> {
    pub unsafe fn assume_valid(self) -> String {
        String::from_utf8_unchecked(self.0)
    }
}

impl MaybeInvalid<FastStr> {
    pub unsafe fn assume_valid(self) -> FastStr {
        FastStr::from_vec_u8_unchecked(self.0)
    }
}

impl<T, S> FromContext<S> for Option<T>
where
    T: FromContext<S>,
    S: Clone + Send + Sync,
{
    type Rejection = Infallible;

    async fn from_context(context: &HttpContext, state: &S) -> Result<Self, Self::Rejection> {
        Ok(T::from_context(context, state).await.ok())
    }
}

impl<S: Sync> FromContext<S> for Address {
    type Rejection = Infallible;

    async fn from_context(context: &HttpContext, _state: &S) -> Result<Address, Self::Rejection> {
        Ok(context.peer.clone())
    }
}

impl<S: Sync> FromContext<S> for Uri {
    type Rejection = Infallible;

    async fn from_context(context: &HttpContext, _state: &S) -> Result<Uri, Self::Rejection> {
        Ok(context.uri.clone())
    }
}

impl<S: Sync> FromContext<S> for Method {
    type Rejection = Infallible;

    async fn from_context(context: &HttpContext, _state: &S) -> Result<Method, Self::Rejection> {
        Ok(context.method.clone())
    }
}

impl<S: Sync> FromContext<S> for Params {
    type Rejection = Infallible;

    async fn from_context(context: &HttpContext, _state: &S) -> Result<Params, Self::Rejection> {
        Ok(context.params.clone())
    }
}

impl<S> FromContext<S> for State<S>
where
    S: Clone + Sync,
{
    type Rejection = Infallible;

    async fn from_context(_context: &HttpContext, state: &S) -> Result<Self, Self::Rejection> {
        Ok(State(state.clone()))
    }
}

impl<T, S> FromContext<S> for Query<T>
where
    T: DeserializeOwned,
    S: Clone + Sync,
{
    type Rejection = RejectionError;

    async fn from_context(context: &HttpContext, _state: &S) -> Result<Self, Self::Rejection> {
        let query = context.uri.query().unwrap_or_default();
        let param = serde_urlencoded::from_str(query).map_err(RejectionError::QueryRejection)?;
        Ok(Query(param))
    }
}

impl<S: Sync> FromContext<S> for ConnectionInfo {
    type Rejection = Infallible;
    async fn from_context(context: &HttpContext, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(context.get_connection_info())
    }
}

impl<S: Sync> FromContext<S> for HeaderMap {
    type Rejection = Infallible;
    async fn from_context(context: &HttpContext, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(context.headers.clone())
    }
}

impl<T, S> FromRequest<S, private::ViaContext> for T
where
    T: FromContext<S> + Sync,
    S: Clone + Send + Sync,
{
    type Rejection = T::Rejection;

    async fn from_request(
        cx: &HttpContext,
        _body: Incoming,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        T::from_context(cx, state).await
    }
}

impl<S: Sync> FromRequest<S> for Incoming {
    type Rejection = Infallible;

    async fn from_request(
        _cx: &HttpContext,
        body: Incoming,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(body)
    }
}

impl<S: Sync> FromRequest<S> for Vec<u8> {
    type Rejection = RejectionError;

    async fn from_request(
        cx: &HttpContext,
        body: Incoming,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Bytes::from_request(cx, body, state).await?.into())
    }
}

impl<S: Sync> FromRequest<S> for Bytes {
    type Rejection = RejectionError;

    async fn from_request(
        cx: &HttpContext,
        body: Incoming,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let bytes = body
            .collect()
            .await
            .map_err(|_| RejectionError::BodyCollectionError)?
            .to_bytes();

        if let Some(Ok(Ok(cap))) = cx
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

impl<S: Sync> FromRequest<S> for String {
    type Rejection = RejectionError;

    async fn from_request(
        cx: &HttpContext,
        body: Incoming,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let vec = Vec::<u8>::from_request(cx, body, state).await?;

        // Check if the &[u8] is a valid string
        let _ = simdutf8::basic::from_utf8(&vec).map_err(RejectionError::StringRejection)?;

        // SAFETY: The `Vec<u8>` is checked by `simdutf8` and it is a valid `String`
        Ok(unsafe { String::from_utf8_unchecked(vec) })
    }
}

impl<S: Sync> FromRequest<S> for FastStr {
    type Rejection = RejectionError;

    async fn from_request(
        cx: &HttpContext,
        body: Incoming,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let vec = Vec::<u8>::from_request(cx, body, state).await?;

        // Check if the &[u8] is a valid string
        let _ = simdutf8::basic::from_utf8(&vec).map_err(RejectionError::StringRejection)?;

        // SAFETY: The `Vec<u8>` is checked by `simdutf8` and it is a valid `String`
        Ok(unsafe { FastStr::from_vec_u8_unchecked(vec) })
    }
}

impl<T, S: Sync> FromRequest<S> for MaybeInvalid<T> {
    type Rejection = RejectionError;

    async fn from_request(
        cx: &HttpContext,
        body: Incoming,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let vec = Vec::<u8>::from_request(cx, body, state).await?;

        Ok(MaybeInvalid(vec, PhantomData))
    }
}

impl<T, S> FromRequest<S> for Form<T>
where
    T: DeserializeOwned,
    S: Sync,
{
    type Rejection = RejectionError;

    async fn from_request(
        cx: &HttpContext,
        body: Incoming,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(cx, body, state).await?;
        let form = serde_html_form::from_bytes::<T>(bytes.as_ref())
            .map_err(RejectionError::FormRejection)?;

        Ok(Form(form))
    }
}

pub enum RejectionError {
    BodyCollectionError,
    InvalidContentType,
    StringRejection(simdutf8::basic::Utf8Error),
    JsonRejection(crate::json::Error),
    QueryRejection(serde_urlencoded::de::Error),
    FormRejection(serde_html_form::de::Error),
}

unsafe impl Send for RejectionError {}

impl IntoResponse for RejectionError {
    fn into_response(self) -> crate::Response {
        let status = match self {
            Self::BodyCollectionError => StatusCode::INTERNAL_SERVER_ERROR,
            Self::InvalidContentType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::StringRejection(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::JsonRejection(_) => StatusCode::BAD_REQUEST,
            Self::QueryRejection(_) => StatusCode::BAD_REQUEST,
            Self::FormRejection(_) => StatusCode::BAD_REQUEST,
        };

        status.into_response()
    }
}
