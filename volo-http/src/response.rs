use std::{
    convert::Infallible,
    ops::{Deref, DerefMut},
    pin::Pin,
    task::{Context, Poll},
};

use futures_util::ready;
use http_body_util::Full;
use hyper::{
    body::{Body, Bytes, Frame},
    http::{response::Builder, StatusCode},
};
use pin_project::pin_project;
use serde::Serialize;

use crate::Json;

pub struct Response(hyper::http::Response<RespBody>);

impl Response {
    pub fn builder() -> Builder {
        Builder::new()
    }

    pub(crate) fn inner(self) -> hyper::http::Response<RespBody> {
        self.0
    }
}

impl Deref for Response {
    type Target = hyper::http::Response<RespBody>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Response {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<hyper::http::Response<RespBody>> for Response {
    fn from(value: hyper::http::Response<RespBody>) -> Self {
        Self(value)
    }
}

#[pin_project]
pub struct RespBody {
    #[pin]
    inner: Full<Bytes>,
}

impl Body for RespBody {
    type Data = Bytes;

    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(ready!(self.project().inner.poll_frame(cx)).map(|result| Ok(result.unwrap())))
    }
}

impl From<Full<Bytes>> for RespBody {
    fn from(value: Full<Bytes>) -> Self {
        Self { inner: value }
    }
}

impl From<Bytes> for RespBody {
    fn from(value: Bytes) -> Self {
        Self {
            inner: Full::new(value),
        }
    }
}

impl From<String> for RespBody {
    fn from(value: String) -> Self {
        Self {
            inner: Full::new(value.into()),
        }
    }
}

impl From<&'static str> for RespBody {
    fn from(value: &'static str) -> Self {
        Self {
            inner: Full::new(value.into()),
        }
    }
}

impl From<()> for RespBody {
    fn from(_: ()) -> Self {
        Self {
            inner: Full::new(Bytes::new()),
        }
    }
}

pub trait IntoResponse {
    fn into_response(self) -> Response;
}

impl IntoResponse for Infallible {
    fn into_response(self) -> Response {
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

impl<T> IntoResponse for T
where
    T: Into<RespBody>,
{
    fn into_response(self) -> Response {
        Response::builder()
            .status(StatusCode::OK)
            .body(self.into())
            .unwrap()
            .into()
    }
}

impl<T> IntoResponse for Json<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        match serde_json::to_string::<T>(&self.0) {
            Ok(s) => s.into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
}

impl<R, E> IntoResponse for Result<R, E>
where
    R: IntoResponse,
    E: IntoResponse,
{
    fn into_response(self) -> Response {
        match self {
            Ok(value) => value.into_response(),
            Err(err) => err.into_response(),
        }
    }
}

impl<T> IntoResponse for (StatusCode, T)
where
    T: IntoResponse,
{
    fn into_response(self) -> Response {
        let mut resp = self.1.into_response();
        *resp.0.status_mut() = self.0;
        resp
    }
}

impl IntoResponse for StatusCode {
    fn into_response(self) -> Response {
        Response::builder()
            .status(self)
            .body(String::new().into())
            .unwrap()
            .into()
    }
}
