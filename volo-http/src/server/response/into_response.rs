use std::{convert::Infallible, error::Error};

use http::{
    header::{HeaderMap, HeaderValue, IntoHeaderName},
    response::Response,
    status::StatusCode,
};

use crate::{body::Body, response::ServerResponse};

pub trait TryIntoResponseHeaders {
    type Error: Error;

    fn try_into_response_headers(self) -> Result<HeaderMap, Self::Error>;
}

pub trait IntoResponse {
    fn into_response(self) -> ServerResponse;
}

impl<K, V> TryIntoResponseHeaders for (K, V)
where
    K: IntoHeaderName,
    V: TryInto<HeaderValue>,
    V::Error: Error,
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
    V::Error: Error,
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
    fn into_response(self) -> ServerResponse {
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

impl<T> IntoResponse for T
where
    T: TryInto<Body>,
    T::Error: IntoResponse,
{
    fn into_response(self) -> ServerResponse {
        let body = match self.try_into() {
            Ok(body) => body,
            Err(e) => {
                return e.into_response();
            }
        };
        Response::builder()
            .status(StatusCode::OK)
            .body(body)
            .unwrap()
    }
}

impl<R, E> IntoResponse for Result<R, E>
where
    R: IntoResponse,
    E: IntoResponse,
{
    fn into_response(self) -> ServerResponse {
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
    fn into_response(self) -> ServerResponse {
        let mut resp = self.1.into_response();
        *resp.status_mut() = self.0;
        resp
    }
}

impl IntoResponse for StatusCode {
    fn into_response(self) -> ServerResponse {
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
    fn into_response(self) -> ServerResponse {
        let (parts, body) = self.into_parts();
        Response::from_parts(parts, body.into())
    }
}

impl<H, R> IntoResponse for (H, R)
where
    H: TryIntoResponseHeaders,
    R: IntoResponse,
{
    fn into_response(self) -> ServerResponse {
        let mut resp = self.1.into_response();
        if let Ok(headers) = self.0.try_into_response_headers() {
            resp.headers_mut().extend(headers);
        }
        resp
    }
}

#[cfg(feature = "form")]
#[cfg_attr(docsrs, doc(cfg(feature = "form")))]
impl<T> IntoResponse for crate::server::extract::Form<T>
where
    T: serde::Serialize,
{
    fn into_response(self) -> ServerResponse {
        let Ok(body) = serde_urlencoded::to_string(&self.0) else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        let body = Body::from(body);

        ServerResponse::builder()
            .status(StatusCode::OK)
            .header(
                http::header::CONTENT_TYPE,
                mime::APPLICATION_WWW_FORM_URLENCODED.essence_str(),
            )
            .body(body)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    }
}

#[cfg(feature = "__json")]
#[cfg_attr(docsrs, doc(cfg(feature = "json")))]
impl<T> IntoResponse for crate::json::Json<T>
where
    T: serde::Serialize,
{
    fn into_response(self) -> ServerResponse {
        let Ok(body) = crate::json::serialize(&self.0) else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        let body = Body::from(body);

        ServerResponse::builder()
            .status(StatusCode::OK)
            .header(
                http::header::CONTENT_TYPE,
                mime::APPLICATION_JSON.essence_str(),
            )
            .body(body)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    }
}
