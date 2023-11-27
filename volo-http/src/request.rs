use bytes::Bytes;
use futures_util::Future;
use http::{header, HeaderMap, Response, StatusCode};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use serde::de::DeserializeOwned;

use crate::{
    extract::FromContext,
    private,
    response::{IntoResponse, RespBody},
    HttpContext,
};

pub trait FromRequest<S, M = private::ViaRequest>: Sized {
    fn from(
        cx: &HttpContext,
        body: Incoming,
        state: &S,
    ) -> impl Future<Output = Result<Self, Response<RespBody>>> + Send;
}

impl<T, S> FromRequest<S, private::ViaContext> for T
where
    T: FromContext<S> + Sync,
    S: Sync,
{
    async fn from(
        cx: &HttpContext,
        _body: Incoming,
        state: &S,
    ) -> Result<Self, Response<RespBody>> {
        match T::from_context(cx, state).await {
            Ok(value) => Ok(value),
            Err(rejection) => Err(rejection.into_response()),
        }
    }
}

impl<S> FromRequest<S> for Incoming
where
    S: Sync,
{
    async fn from(
        _cx: &HttpContext,
        body: Incoming,
        _state: &S,
    ) -> Result<Self, Response<RespBody>> {
        Ok(body)
    }
}

pub struct Json<T>(pub T);

impl<T: DeserializeOwned, S> FromRequest<S> for Json<T> {
    fn from(
        cx: &HttpContext,
        body: Incoming,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Response<RespBody>>> + Send {
        async move {
            if !json_content_type(&cx.headers) {
                return Err(Response::builder()
                    .status(StatusCode::UNSUPPORTED_MEDIA_TYPE)
                    .body(Bytes::new().into())
                    .unwrap());
            }

            match body.collect().await {
                Ok(body) => {
                    let body = body.to_bytes();
                    match serde_json::from_slice::<T>(body.as_ref()) {
                        Ok(t) => Ok(Self(t)),
                        Err(e) => {
                            tracing::warn!("json serialization error {e}");
                            Err(Response::builder()
                                .status(StatusCode::BAD_REQUEST)
                                .body(Bytes::new().into())
                                .unwrap())
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("collect body error: {e}");
                    Err(Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Bytes::new().into())
                        .unwrap())
                }
            }
        }
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
