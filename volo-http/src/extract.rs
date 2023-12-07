use std::{
    convert::Infallible,
    fmt,
    ops::{Deref, DerefMut},
};

use bytes::{BufMut, Bytes};
use futures_util::Future;
use http_body_util::BodyExt;
use hyper::{
    body::Incoming,
    http::{header, HeaderMap, Method, StatusCode, Uri},
};
use serde::de::DeserializeOwned;
use volo::net::Address;

use crate::{param::Params, response::IntoResponse, HttpContext};

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

    fn from(
        cx: &HttpContext,
        body: Incoming,
        state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct State<S>(pub S);

pub struct Json<T>(pub T);

pub struct UTF8String(pub String);

#[derive(Debug, Default, Clone)]
pub struct UncheckedString(pub String);

impl fmt::Display for UTF8String {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Deref for UTF8String {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for UTF8String {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
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

impl<T, S> FromRequest<S, private::ViaContext> for T
where
    T: FromContext<S> + Sync,
    S: Clone + Send + Sync,
{
    type Rejection = T::Rejection;

    async fn from(cx: &HttpContext, _body: Incoming, state: &S) -> Result<Self, Self::Rejection> {
        T::from_context(cx, state).await
    }
}

impl<S: Sync> FromRequest<S> for Incoming {
    type Rejection = Infallible;

    async fn from(_cx: &HttpContext, body: Incoming, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(body)
    }
}

impl<S: Sync> FromRequest<S> for Bytes {
    type Rejection = BytesRejection;

    async fn from(
        cx: &HttpContext,
        mut body: Incoming,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        if let Some(Ok(Ok(cap))) = cx
            .headers
            .get(header::CONTENT_LENGTH)
            .map(|v| v.to_str().map(|c| c.parse()))
        {
            if cap == 0 {
                return Ok(Bytes::new());
            }
            let mut vec = Vec::with_capacity(cap);
            while let Some(next) = body.frame().await {
                let frame = next.map_err(|_| BytesRejection::BodyCollectionError)?;
                let bytes = frame
                    .into_data()
                    .map_err(|_| BytesRejection::BodyCollectionError)?;
                vec.put(bytes);
            }
            return Ok(vec.into());
        }

        // fallback
        match body.collect().await {
            Ok(col) => Ok(col.to_bytes()),
            Err(_) => Err(BytesRejection::BodyCollectionError),
        }
    }
}

impl<S: Sync> FromRequest<S> for UTF8String {
    type Rejection = StringRejection;

    async fn from(cx: &HttpContext, body: Incoming, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = <Bytes as FromRequest<S>>::from(cx, body, state)
            .await
            .map_err(|_| StringRejection::BodyCollectionError)?;
        Ok(UTF8String(
            String::from_utf8(bytes.to_vec()).map_err(StringRejection::StringDecodeError)?,
        ))
    }
}

impl<S: Sync> FromRequest<S> for UncheckedString {
    type Rejection = StringRejection;

    async fn from(cx: &HttpContext, body: Incoming, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = <Bytes as FromRequest<S>>::from(cx, body, state)
            .await
            .map_err(|_| StringRejection::BodyCollectionError)?;

        Ok(UncheckedString(unsafe {
            String::from_utf8_unchecked(bytes.to_vec())
        }))
    }
}

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Sync,
{
    type Rejection = JsonRejection;

    async fn from(cx: &HttpContext, body: Incoming, _state: &S) -> Result<Self, Self::Rejection> {
        if !json_content_type(&cx.headers) {
            return Err(JsonRejection::MissingJsonContentType);
        }

        let body = body
            .collect()
            .await
            .map_err(|_| JsonRejection::BodyCollectionError)?;
        let bytes = body.to_bytes();
        let json = serde_json::from_slice::<T>(bytes.as_ref())
            .map_err(JsonRejection::SerializationError)?;

        Ok(Json(json))
    }
}

pub enum BytesRejection {
    BodyCollectionError,
}

unsafe impl Send for BytesRejection {}

impl IntoResponse for BytesRejection {
    fn into_response(self) -> crate::Response {
        let status = match self {
            Self::BodyCollectionError => StatusCode::INTERNAL_SERVER_ERROR,
        };

        status.into_response()
    }
}

pub enum StringRejection {
    BodyCollectionError,
    StringDecodeError(std::string::FromUtf8Error),
}

unsafe impl Send for StringRejection {}

impl IntoResponse for StringRejection {
    fn into_response(self) -> crate::Response {
        let status = match self {
            Self::BodyCollectionError => StatusCode::INTERNAL_SERVER_ERROR,
            Self::StringDecodeError(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        };

        status.into_response()
    }
}

pub enum JsonRejection {
    MissingJsonContentType,
    SerializationError(serde_json::Error),
    BodyCollectionError,
}

unsafe impl Send for JsonRejection {}

impl IntoResponse for JsonRejection {
    fn into_response(self) -> crate::Response {
        let status = match self {
            Self::MissingJsonContentType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::SerializationError(_) => StatusCode::BAD_REQUEST,
            Self::BodyCollectionError => StatusCode::INTERNAL_SERVER_ERROR,
        };

        status.into_response()
    }
}

fn json_content_type(headers: &HeaderMap) -> bool {
    let content_type = if let Some(content_type) = headers.get(header::CONTENT_TYPE) {
        content_type
    } else {
        return false;
    };

    let content_type = if let Ok(content_type) = content_type.to_str() {
        content_type
    } else {
        return false;
    };

    let mime = if let Ok(mime) = content_type.parse::<mime::Mime>() {
        mime
    } else {
        return false;
    };

    let is_json_content_type = mime.type_() == "application"
        && (mime.subtype() == "json" || mime.suffix().map_or(false, |name| name == "json"));

    is_json_content_type
}
