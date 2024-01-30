use std::convert::Infallible;

use http::{
    header::{HeaderValue, IntoHeaderName},
    HeaderMap, StatusCode,
};

use crate::body::Body;

pub type Response<B = Body> = http::Response<B>;

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
    T: Into<Body>,
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
    B: Into<Body>,
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
