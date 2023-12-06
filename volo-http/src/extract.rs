use std::convert::Infallible;

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

        match body.collect().await {
            Ok(body) => {
                let body = body.to_bytes();
                match serde_json::from_slice::<T>(body.as_ref()) {
                    Ok(t) => Ok(Self(t)),
                    Err(e) => {
                        tracing::warn!("json serialization error {e}");
                        Err(JsonRejection::SerializationError)
                    }
                }
            }
            Err(e) => {
                tracing::warn!("collect body error: {e}");
                Err(JsonRejection::BodyCollectionError)
            }
        }
    }
}

pub enum JsonRejection {
    MissingJsonContentType,
    SerializationError,
    BodyCollectionError,
}

unsafe impl Send for JsonRejection {}

impl IntoResponse for JsonRejection {
    fn into_response(self) -> crate::Response {
        let status = match self {
            Self::MissingJsonContentType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::SerializationError => StatusCode::BAD_REQUEST,
            Self::BodyCollectionError => StatusCode::BAD_REQUEST,
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
