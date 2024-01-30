use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use faststr::FastStr;
use futures_util::ready;
use http::{
    header::{HeaderValue, IntoHeaderName},
    HeaderMap, StatusCode,
};
use http_body::{Body, Frame, SizeHint};
use http_body_util::Full;
use pin_project::pin_project;

pub type Response<B = RespBody> = http::Response<B>;

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

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl From<Full<Bytes>> for RespBody {
    fn from(value: Full<Bytes>) -> Self {
        Self { inner: value }
    }
}

impl From<Vec<u8>> for RespBody {
    fn from(value: Vec<u8>) -> Self {
        Self {
            inner: Full::new(value.into()),
        }
    }
}

impl From<Bytes> for RespBody {
    fn from(value: Bytes) -> Self {
        Self {
            inner: Full::new(value),
        }
    }
}

impl From<FastStr> for RespBody {
    fn from(value: FastStr) -> Self {
        Self {
            inner: Full::new(value.into()),
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

pub trait TryIntoResponseHeaders {
    type Error;

    fn try_into_response_headers(self) -> Result<HeaderMap, Self::Error>;
}

pub trait IntoResponse {
    fn into_response(self) -> Response;
}

impl<K, V> TryIntoResponseHeaders for (K, V)
where
    K: IntoHeaderName,
    V: TryInto<HeaderValue>,
{
    type Error = V::Error;

    fn try_into_response_headers(self) -> Result<HeaderMap, Self::Error> {
        let mut headers = HeaderMap::with_capacity(1);
        headers.insert(self.0, self.1.try_into()?);
        Ok(headers)
    }
}

impl<K, V, const N: usize> TryIntoResponseHeaders for [(K, V); N]
where
    K: IntoHeaderName,
    V: TryInto<HeaderValue>,
{
    type Error = V::Error;

    fn try_into_response_headers(self) -> Result<HeaderMap, Self::Error> {
        let mut headers = HeaderMap::with_capacity(N);
        for (k, v) in self.into_iter() {
            headers.insert(k, v.try_into()?);
        }
        Ok(headers)
    }
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
        *resp.status_mut() = self.0;
        resp
    }
}

impl IntoResponse for StatusCode {
    fn into_response(self) -> Response {
        Response::builder()
            .status(self)
            .body(String::new().into())
            .unwrap()
    }
}

impl<B> IntoResponse for http::Response<B>
where
    B: Into<RespBody>,
{
    fn into_response(self) -> Response {
        let (parts, body) = self.into_parts();
        Response::from_parts(parts, body.into())
    }
}

impl<H, R> IntoResponse for (H, R)
where
    H: TryIntoResponseHeaders,
    R: IntoResponse,
{
    fn into_response(self) -> Response {
        let mut resp = self.1.into_response();
        if let Ok(headers) = self.0.try_into_response_headers() {
            resp.headers_mut().extend(headers);
        }
        resp
    }
}
