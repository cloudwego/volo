use std::convert::Infallible;

use bytes::Bytes;
use futures_util::Future;
use http_body_util::BodyExt;
use hyper::{
    body::Incoming,
    http::{header, HeaderMap, Method, StatusCode, Uri},
};
use serde::de::DeserializeOwned;
use volo::net::Address;

use crate::{
    param::Params,
    response::{IntoResponse, Response},
    HttpContext,
};

mod private {
    #[derive(Debug, Clone, Copy)]
    pub enum ViaContext {}

    #[derive(Debug, Clone, Copy)]
    pub enum ViaRequest {}
}

pub trait FromContext<S>: Sized {
    fn from_context(
        context: &HttpContext,
        state: &S,
    ) -> impl Future<Output = Result<Self, Infallible>> + Send;
}

pub trait FromRequest<S, M = private::ViaRequest>: Sized {
    fn from(
        cx: &HttpContext,
        body: Incoming,
        state: &S,
    ) -> impl Future<Output = Result<Self, Response>> + Send;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct State<S>(pub S);

pub struct Json<T>(pub T);

impl<T, S> FromContext<S> for Option<T>
where
    T: FromContext<S>,
    S: Send + Sync,
{
    async fn from_context(context: &HttpContext, state: &S) -> Result<Self, Infallible> {
        Ok(T::from_context(context, state).await.ok())
    }
}

impl<S> FromContext<S> for Address
where
    S: Send + Sync,
{
    async fn from_context(context: &HttpContext, _state: &S) -> Result<Address, Infallible> {
        Ok(context.peer.clone())
    }
}

impl<S> FromContext<S> for Uri
where
    S: Send + Sync,
{
    async fn from_context(context: &HttpContext, _state: &S) -> Result<Uri, Infallible> {
        Ok(context.uri.clone())
    }
}

impl<S> FromContext<S> for Method
where
    S: Send + Sync,
{
    async fn from_context(context: &HttpContext, _state: &S) -> Result<Method, Infallible> {
        Ok(context.method.clone())
    }
}

impl<S> FromContext<S> for Params
where
    S: Send + Sync,
{
    async fn from_context(context: &HttpContext, _state: &S) -> Result<Params, Infallible> {
        Ok(context.params.clone())
    }
}

impl<S> FromContext<S> for State<S>
where
    S: Clone + Send + Sync,
{
    async fn from_context(_context: &HttpContext, state: &S) -> Result<Self, Infallible> {
        Ok(State(state.clone()))
    }
}

impl<T, S> FromRequest<S, private::ViaContext> for T
where
    T: FromContext<S> + Sync,
    S: Sync,
{
    async fn from(cx: &HttpContext, _body: Incoming, state: &S) -> Result<Self, Response> {
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
    async fn from(_cx: &HttpContext, body: Incoming, _state: &S) -> Result<Self, Response> {
        Ok(body)
    }
}

impl<T: DeserializeOwned, S> FromRequest<S> for Json<T> {
    fn from(
        cx: &HttpContext,
        body: Incoming,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Response>> + Send {
        async move {
            if !json_content_type(&cx.headers) {
                return Err(Response::builder()
                    .status(StatusCode::UNSUPPORTED_MEDIA_TYPE)
                    .body(Bytes::new().into())
                    .unwrap()
                    .into());
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
                                .unwrap()
                                .into())
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("collect body error: {e}");
                    Err(Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Bytes::new().into())
                        .unwrap()
                        .into())
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
