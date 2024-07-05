use std::{convert::Infallible, marker::PhantomData};

use bytes::Bytes;
use faststr::FastStr;
use futures_util::Future;
use http::{header, request::Parts, Method, Request, Uri};
use http_body::Body;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use volo::{context::Context, net::Address};

use super::IntoResponse;
use crate::{
    context::ServerContext,
    error::server::{body_collection_error, ExtractBodyError},
};

mod private {
    #[derive(Debug, Clone, Copy)]
    pub enum ViaContext {}

    #[derive(Debug, Clone, Copy)]
    pub enum ViaRequest {}
}

pub trait FromContext: Sized {
    type Rejection: IntoResponse;

    fn from_context(
        cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send;
}

pub trait FromRequest<B = Incoming, M = private::ViaRequest>: Sized {
    type Rejection: IntoResponse;

    fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: B,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Query<T>(pub T);

#[derive(Debug, Default, Clone, Copy)]
pub struct Form<T>(pub T);

#[derive(Debug, Default, Clone)]
pub struct MaybeInvalid<T>(Vec<u8>, PhantomData<T>);

impl MaybeInvalid<String> {
    /// # Safety
    ///
    /// It is up to the caller to guarantee that the value really is valid. Using this when the
    /// content is invalid causes immediate undefined behavior.
    pub unsafe fn assume_valid(self) -> String {
        String::from_utf8_unchecked(self.0)
    }
}

impl MaybeInvalid<FastStr> {
    /// # Safety
    ///
    /// It is up to the caller to guarantee that the value really is valid. Using this when the
    /// content is invalid causes immediate undefined behavior.
    pub unsafe fn assume_valid(self) -> FastStr {
        FastStr::from_vec_u8_unchecked(self.0)
    }
}

impl<T> FromContext for Option<T>
where
    T: FromContext,
{
    type Rejection = Infallible;

    async fn from_context(
        cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> Result<Self, Self::Rejection> {
        Ok(T::from_context(cx, parts).await.ok())
    }
}

impl<T> FromContext for Result<T, T::Rejection>
where
    T: FromContext,
{
    type Rejection = Infallible;

    async fn from_context(
        cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> Result<Self, Self::Rejection> {
        Ok(T::from_context(cx, parts).await)
    }
}

impl FromContext for Address {
    type Rejection = Infallible;

    async fn from_context(
        cx: &mut ServerContext,
        _parts: &mut Parts,
    ) -> Result<Address, Self::Rejection> {
        Ok(cx
            .rpc_info()
            .caller()
            .address()
            .expect("server context does not have caller address"))
    }
}

impl FromContext for Uri {
    type Rejection = Infallible;

    async fn from_context(
        _cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> Result<Uri, Self::Rejection> {
        Ok(parts.uri.to_owned())
    }
}

impl FromContext for Method {
    type Rejection = Infallible;

    async fn from_context(
        _cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> Result<Method, Self::Rejection> {
        Ok(parts.method.to_owned())
    }
}

#[cfg(feature = "query")]
impl<T> FromContext for Query<T>
where
    T: serde::de::DeserializeOwned,
{
    type Rejection = serde_urlencoded::de::Error;

    async fn from_context(
        _cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> Result<Self, Self::Rejection> {
        let query = parts.uri.query().unwrap_or_default();
        let param = serde_urlencoded::from_str(query)?;
        Ok(Query(param))
    }
}

#[cfg(feature = "query")]
impl IntoResponse for serde_urlencoded::de::Error {
    fn into_response(self) -> crate::response::ServerResponse {
        http::StatusCode::BAD_REQUEST.into_response()
    }
}

impl<B, T> FromRequest<B, private::ViaContext> for T
where
    B: Send,
    T: FromContext + Sync,
{
    type Rejection = T::Rejection;

    async fn from_request(
        cx: &mut ServerContext,
        mut parts: Parts,
        _: B,
    ) -> Result<Self, Self::Rejection> {
        T::from_context(cx, &mut parts).await
    }
}

impl<B, T> FromRequest<B> for Option<T>
where
    B: Send,
    T: FromRequest<B, private::ViaRequest> + Sync,
{
    type Rejection = Infallible;

    async fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: B,
    ) -> Result<Self, Self::Rejection> {
        Ok(T::from_request(cx, parts, body).await.ok())
    }
}

impl<B, T> FromRequest<B> for Result<T, T::Rejection>
where
    B: Send,
    T: FromRequest<B, private::ViaRequest> + Sync,
{
    type Rejection = Infallible;

    async fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: B,
    ) -> Result<Self, Self::Rejection> {
        Ok(T::from_request(cx, parts, body).await)
    }
}

impl<B> FromRequest<B> for Request<B>
where
    B: Send,
{
    type Rejection = Infallible;

    async fn from_request(
        _cx: &mut ServerContext,
        parts: Parts,
        body: B,
    ) -> Result<Self, Self::Rejection> {
        Ok(Request::from_parts(parts, body))
    }
}

impl<B> FromRequest<B> for Vec<u8>
where
    B: Body + Send,
    B::Data: Send,
    B::Error: Send,
{
    type Rejection = ExtractBodyError;

    async fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: B,
    ) -> Result<Self, Self::Rejection> {
        Ok(Bytes::from_request(cx, parts, body).await?.into())
    }
}

impl<B> FromRequest<B> for Bytes
where
    B: Body + Send,
    B::Data: Send,
    B::Error: Send,
{
    type Rejection = ExtractBodyError;

    async fn from_request(
        _: &mut ServerContext,
        parts: Parts,
        body: B,
    ) -> Result<Self, Self::Rejection> {
        let bytes = body
            .collect()
            .await
            .map_err(|_| body_collection_error())?
            .to_bytes();

        if let Some(Ok(Ok(cap))) = parts
            .headers
            .get(header::CONTENT_LENGTH)
            .map(|v| v.to_str().map(|c| c.parse::<usize>()))
        {
            if bytes.len() != cap {
                tracing::warn!(
                    "[Volo-HTTP] The length of body ({}) does not match the Content-Length ({})",
                    bytes.len(),
                    cap
                );
            }
        }

        Ok(bytes)
    }
}

impl<B> FromRequest<B> for String
where
    B: Body + Send,
    B::Data: Send,
    B::Error: Send,
{
    type Rejection = ExtractBodyError;

    async fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: B,
    ) -> Result<Self, Self::Rejection> {
        let vec = Vec::<u8>::from_request(cx, parts, body).await?;

        // Check if the &[u8] is a valid string
        let _ = simdutf8::basic::from_utf8(&vec).map_err(ExtractBodyError::String)?;

        // SAFETY: The `Vec<u8>` is checked by `simdutf8` and it is a valid `String`
        Ok(unsafe { String::from_utf8_unchecked(vec) })
    }
}

impl<B> FromRequest<B> for FastStr
where
    B: Body + Send,
    B::Data: Send,
    B::Error: Send,
{
    type Rejection = ExtractBodyError;

    async fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: B,
    ) -> Result<Self, Self::Rejection> {
        let vec = Vec::<u8>::from_request(cx, parts, body).await?;

        // Check if the &[u8] is a valid string
        let _ = simdutf8::basic::from_utf8(&vec).map_err(ExtractBodyError::String)?;

        // SAFETY: The `Vec<u8>` is checked by `simdutf8` and it is a valid `String`
        Ok(unsafe { FastStr::from_vec_u8_unchecked(vec) })
    }
}

impl<B, T> FromRequest<B> for MaybeInvalid<T>
where
    B: Body + Send,
    B::Data: Send,
    B::Error: Send,
{
    type Rejection = ExtractBodyError;

    async fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: B,
    ) -> Result<Self, Self::Rejection> {
        let vec = Vec::<u8>::from_request(cx, parts, body).await?;

        Ok(MaybeInvalid(vec, PhantomData))
    }
}

#[cfg(feature = "form")]
impl<B, T> FromRequest<B> for Form<T>
where
    B: Body + Send,
    B::Data: Send,
    B::Error: Send,
    T: serde::de::DeserializeOwned,
{
    type Rejection = ExtractBodyError;

    async fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: B,
    ) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(cx, parts, body).await?;
        let form =
            serde_urlencoded::from_bytes::<T>(bytes.as_ref()).map_err(ExtractBodyError::Form)?;

        Ok(Form(form))
    }
}

#[cfg(test)]
mod extract_tests {
    #![deny(unused)]

    use std::convert::Infallible;

    use http::request::Parts;
    use hyper::body::Incoming;

    use super::{FromContext, FromRequest};
    use crate::{context::ServerContext, server::handler::Handler};

    struct SomethingFromCx;

    impl FromContext for SomethingFromCx {
        type Rejection = Infallible;
        async fn from_context(
            _: &mut ServerContext,
            _: &mut Parts,
        ) -> Result<Self, Self::Rejection> {
            unimplemented!()
        }
    }

    struct SomethingFromReq;

    impl FromRequest for SomethingFromReq {
        type Rejection = Infallible;
        async fn from_request(
            _: &mut ServerContext,
            _: Parts,
            _: Incoming,
        ) -> Result<Self, Self::Rejection> {
            unimplemented!()
        }
    }

    #[test]
    fn extractor() {
        fn assert_handler<H, T>(_: H)
        where
            H: Handler<T, Incoming, Infallible>,
        {
        }

        async fn only_cx(_: SomethingFromCx) {}
        async fn only_req(_: SomethingFromReq) {}
        async fn cx_and_req(_: SomethingFromCx, _: SomethingFromReq) {}
        async fn many_cx_and_req(
            _: SomethingFromCx,
            _: SomethingFromCx,
            _: SomethingFromCx,
            _: SomethingFromReq,
        ) {
        }
        async fn only_option_cx(_: Option<SomethingFromCx>) {}
        async fn only_option_req(_: Option<SomethingFromReq>) {}
        async fn only_result_cx(_: Result<SomethingFromCx, Infallible>) {}
        async fn only_result_req(_: Result<SomethingFromReq, Infallible>) {}
        async fn option_cx_req(_: Option<SomethingFromCx>, _: Option<SomethingFromReq>) {}
        async fn result_cx_req(
            _: Result<SomethingFromCx, Infallible>,
            _: Result<SomethingFromReq, Infallible>,
        ) {
        }

        assert_handler(only_cx);
        assert_handler(only_req);
        assert_handler(cx_and_req);
        assert_handler(many_cx_and_req);
        assert_handler(only_option_cx);
        assert_handler(only_option_req);
        assert_handler(only_result_cx);
        assert_handler(only_result_req);
        assert_handler(option_cx_req);
        assert_handler(result_cx_req);
    }
}
