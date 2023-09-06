use bytes::Bytes;
use futures_util::Future;
use http::{header, HeaderMap, Response, StatusCode};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use serde::de::DeserializeOwned;

<<<<<<< HEAD
<<<<<<< HEAD
=======
>>>>>>> handler, extractor (#221)
use crate::{
    extract::FromContext,
    response::{IntoResponse, RespBody},
    HttpContext,
};
<<<<<<< HEAD
=======
use crate::{response::RespBody, HttpContext};
>>>>>>> init
=======
>>>>>>> handler, extractor (#221)

pub trait FromRequest: Sized {
    type FromFut<'cx>: Future<Output = Result<Self, Response<RespBody>>> + Send + 'cx
    where
        Self: 'cx;

    fn from(cx: &HttpContext, body: Incoming) -> Self::FromFut<'_>;
}

<<<<<<< HEAD
<<<<<<< HEAD
=======
>>>>>>> handler, extractor (#221)
impl<T> FromRequest for T
where
    T: FromContext,
{
    type FromFut<'cx> = impl Future<Output = Result<Self, Response<RespBody>>> + Send + 'cx
        where
            Self: 'cx;

    fn from(cx: &HttpContext, _body: Incoming) -> Self::FromFut<'_> {
        async move {
            match T::from_context(cx).await {
                Ok(value) => Ok(value),
                Err(rejection) => Err(rejection.into_response()),
            }
        }
    }
}

<<<<<<< HEAD
=======
>>>>>>> init
=======
>>>>>>> handler, extractor (#221)
impl FromRequest for Incoming {
    type FromFut<'cx> = impl Future<Output = Result<Self, Response<RespBody>>> + Send + 'cx
    where
        Self: 'cx;

    fn from(_cx: &HttpContext, body: Incoming) -> Self::FromFut<'_> {
        async { Ok(body) }
    }
}

pub struct Json<T>(pub T);

impl<T: DeserializeOwned> FromRequest for Json<T> {
    type FromFut<'cx> = impl Future<Output = Result<Self, Response<RespBody>>> + Send + 'cx
    where
        Self: 'cx;

    fn from(cx: &HttpContext, body: Incoming) -> Self::FromFut<'_> {
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
