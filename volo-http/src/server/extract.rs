//! Traits and types for extracting data from [`ServerContext`] and [`ServerRequest`]
//!
//! See [`FromContext`] and [`FromRequest`] for more details.

use std::{convert::Infallible, fmt, marker::PhantomData};

use bytes::Bytes;
use faststr::FastStr;
use futures_util::Future;
use http::{
    header::{self, HeaderMap, HeaderName},
    method::Method,
    request::Parts,
    status::StatusCode,
    uri::{Scheme, Uri},
};
use http_body::Body;
use http_body_util::BodyExt;
use volo::{context::Context, net::Address};

use super::IntoResponse;
use crate::{
    context::ServerContext,
    error::server::{body_collection_error, ExtractBodyError},
    request::{RequestPartsExt, ServerRequest},
    server::layer::ClientIP,
    utils::macros::impl_deref_and_deref_mut,
};

mod private {
    #[derive(Debug, Clone, Copy)]
    pub enum ViaContext {}

    #[derive(Debug, Clone, Copy)]
    pub enum ViaRequest {}
}

/// Extract a type from context ([`ServerContext`] and [`Parts`])
///
/// This trait is used for handlers, which can extract something from [`ServerContext`] and
/// [`ServerRequest`].
///
/// [`FromContext`] only borrows [`ServerContext`] and [`Parts`]. If your extractor needs to
/// consume [`Parts`] or the whole [`ServerRequest`], please use [`FromRequest`] instead.
pub trait FromContext: Sized {
    /// If the extractor fails, it will return this `Rejection` type.
    ///
    /// The `Rejection` should implement [`IntoResponse`]. If extractor fails in handler, the
    /// rejection will be converted into a [`ServerResponse`](crate::response::ServerResponse) and
    /// returned.
    type Rejection: IntoResponse;

    /// Extract the type from context.
    fn from_context(
        cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send;
}

/// Extract a type from [`ServerRequest`] with its [`ServerContext`]
///
/// This trait is used for handlers, which can extract something from [`ServerContext`] and
/// [`ServerRequest`].
///
/// [`FromRequest`] will consume [`ServerRequest`], so it can only be used once in a handler. If
/// your extractor does not need to consume [`ServerRequest`], please use [`FromContext`] instead.
pub trait FromRequest<B = crate::body::Body, M = private::ViaRequest>: Sized {
    /// If the extractor fails, it will return this `Rejection` type.
    ///
    /// The `Rejection` should implement [`IntoResponse`]. If extractor fails in handler, the
    /// rejection will be converted into a [`ServerResponse`](crate::response::ServerResponse) and
    /// returned.
    type Rejection: IntoResponse;

    /// Extract the type from request.
    fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: B,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send;
}

/// Extract a type from query in uri.
///
/// Note that the type must implement [`Deserialize`](serde::Deserialize).
#[cfg(feature = "query")]
#[derive(Debug, Default, Clone, Copy)]
pub struct Query<T>(pub T);

/// Extract a type from a urlencoded body.
///
/// Note that the type must implement [`Deserialize`](serde::Deserialize).
#[cfg(feature = "form")]
#[derive(Debug, Default, Clone, Copy)]
pub struct Form<T>(pub T);

/// A wrapper that can extract a type from a json body or convert a type to json response.
///
/// # Examples
///
/// Use [`Json`] as parameter:
///
/// ```
/// use serde::Deserialize;
/// use volo_http::server::{
///     extract::Json,
///     route::{post, Router},
/// };
///
/// #[derive(Debug, Deserialize)]
/// struct User {
///     username: String,
///     password: String,
/// }
///
/// async fn login(Json(user): Json<User>) {
///     println!("user: {user:?}");
/// }
///
/// let router: Router = Router::new().route("/api/v2/login", post(login));
/// ```
///
/// User [`Json`] as response:
///
/// ```
/// use serde::Serialize;
/// use volo_http::server::{
///     extract::Json,
///     route::{get, Router},
/// };
///
/// #[derive(Debug, Serialize)]
/// struct User {
///     username: String,
///     password: String,
/// }
///
/// async fn user_info() -> Json<User> {
///     let user = User {
///         username: String::from("admin"),
///         password: String::from("passw0rd"),
///     };
///     Json(user)
/// }
///
/// let router: Router = Router::new().route("/api/v2/info", get(user_info));
/// ```
#[cfg(feature = "json")]
#[derive(Debug, Default, Clone, Copy)]
pub struct Json<T>(pub T);

/// Extract a [`String`] or [`FastStr`] without checking.
///
/// This type can extract a [`String`] or [`FastStr`] like [`String::from_utf8_unchecked`] or
/// [`FastStr::from_vec_u8_unchecked`]. Note that extracting them is unsafe and users should assume
/// that the value is valid.
#[derive(Debug, Default, Clone)]
pub struct MaybeInvalid<T>(Vec<u8>, PhantomData<T>);

impl MaybeInvalid<String> {
    /// Assume the [`String`] is valid and extract it without checking.
    ///
    /// # Safety
    ///
    /// It is up to the caller to guarantee that the value really is valid. Using this when the
    /// content is invalid causes immediate undefined behavior.
    pub unsafe fn assume_valid(self) -> String {
        String::from_utf8_unchecked(self.0)
    }
}

impl MaybeInvalid<FastStr> {
    /// Assume the [`FastStr`] is valid and extract it without checking.
    ///
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

/// Full uri including scheme, host, path and query.
#[derive(Debug)]
pub struct FullUri(Uri);

impl_deref_and_deref_mut!(FullUri, Uri, 0);

impl fmt::Display for FullUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromContext for FullUri {
    type Rejection = http::Error;

    async fn from_context(
        cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> Result<Self, Self::Rejection> {
        let scheme = if is_tls(cx) {
            Scheme::HTTPS
        } else {
            Scheme::HTTP
        };
        Uri::builder()
            .scheme(scheme)
            .authority(parts.host().map(ToOwned::to_owned).unwrap_or_default())
            .path_and_query(
                parts
                    .uri
                    .path_and_query()
                    .map(ToString::to_string)
                    .unwrap_or(String::from("/")),
            )
            .build()
            .map(FullUri)
    }
}

impl IntoResponse for http::Error {
    fn into_response(self) -> crate::response::ServerResponse {
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
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

impl FromContext for ClientIP {
    type Rejection = Infallible;

    async fn from_context(
        cx: &mut ServerContext,
        _: &mut Parts,
    ) -> Result<ClientIP, Self::Rejection> {
        if let Some(client_ip) = cx.rpc_info.caller().tags.get::<ClientIP>() {
            match client_ip.0 {
                Some(ip) => Ok(ClientIP(Some(ip))),
                None => Ok(ClientIP(None)),
            }
        } else {
            Ok(ClientIP(None))
        }
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
        StatusCode::BAD_REQUEST.into_response()
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

impl<B> FromRequest<B> for ServerRequest<B>
where
    B: Send,
{
    type Rejection = Infallible;

    async fn from_request(
        _cx: &mut ServerContext,
        parts: Parts,
        body: B,
    ) -> Result<Self, Self::Rejection> {
        Ok(ServerRequest::from_parts(parts, body))
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

        if let Some(cap) = get_header_value(&parts.headers, header::CONTENT_LENGTH) {
            if let Ok(cap) = cap.parse::<usize>() {
                if bytes.len() != cap {
                    tracing::warn!(
                        "[Volo-HTTP] The length of body ({}) does not match the Content-Length \
                         ({})",
                        bytes.len(),
                        cap,
                    );
                }
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
        if !content_type_eq(&parts.headers, mime::APPLICATION_WWW_FORM_URLENCODED) {
            return Err(crate::error::server::invalid_content_type());
        }

        let bytes = Bytes::from_request(cx, parts, body).await?;
        let form =
            serde_urlencoded::from_bytes::<T>(bytes.as_ref()).map_err(ExtractBodyError::Form)?;

        Ok(Form(form))
    }
}

#[cfg(feature = "json")]
impl<B, T> FromRequest<B> for Json<T>
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
        if !json_content_type(&parts.headers) {
            return Err(crate::error::server::invalid_content_type());
        }

        let bytes = Bytes::from_request(cx, parts, body).await?;
        let json = crate::utils::json::deserialize(&bytes).map_err(ExtractBodyError::Json)?;

        Ok(Json(json))
    }
}

#[cfg(not(feature = "__tls"))]
fn is_tls(_: &ServerContext) -> bool {
    false
}

#[cfg(feature = "__tls")]
fn is_tls(cx: &ServerContext) -> bool {
    cx.rpc_info().config().is_tls()
}

fn get_header_value(map: &HeaderMap, key: HeaderName) -> Option<&str> {
    map.get(key)?.to_str().ok()
}

#[cfg(feature = "form")]
fn content_type_eq(map: &HeaderMap, val: mime::Mime) -> bool {
    let Some(ty) = get_header_value(map, header::CONTENT_TYPE) else {
        return false;
    };
    ty == val.essence_str()
}

#[cfg(feature = "json")]
fn json_content_type(headers: &HeaderMap) -> bool {
    let content_type = match headers.get(header::CONTENT_TYPE) {
        Some(content_type) => content_type,
        None => {
            return false;
        }
    };

    let content_type = match content_type.to_str() {
        Ok(s) => s,
        Err(_) => {
            return false;
        }
    };

    let mime_type = match content_type.parse::<mime::Mime>() {
        Ok(mime_type) => mime_type,
        Err(_) => {
            return false;
        }
    };

    // `application/json` or `application/json+foo`
    if mime_type.type_() == mime::APPLICATION && mime_type.subtype() == mime::JSON {
        return true;
    }

    // `application/foo+json`
    if mime_type.suffix() == Some(mime::JSON) {
        return true;
    }

    false
}

#[cfg(test)]
mod extract_tests {
    #![deny(unused)]

    use std::convert::Infallible;

    use http::request::Parts;

    use super::{FromContext, FromRequest};
    use crate::{body::Body, context::ServerContext, server::handler::Handler};

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
            _: Body,
        ) -> Result<Self, Self::Rejection> {
            unimplemented!()
        }
    }

    #[test]
    fn extractor() {
        fn assert_handler<H, T>(_: H)
        where
            H: Handler<T, Body, Infallible>,
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
