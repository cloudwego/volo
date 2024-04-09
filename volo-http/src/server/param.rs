use std::{convert::Infallible, error::Error, fmt, ops::Deref, str::FromStr};

use ahash::AHashMap;
use bytes::{BufMut, BytesMut};
use faststr::FastStr;
use http::{request::Parts, StatusCode};
use matchit::Params;

use super::{extract::FromContext, IntoResponse};
use crate::{
    context::ServerContext, error::BoxError, response::ServerResponse,
    utils::macros::all_the_tuples,
};

#[derive(Clone, Debug, Default)]
pub struct UrlParamsVec {
    inner: Vec<(FastStr, FastStr)>,
}

impl UrlParamsVec {
    pub(crate) fn extend(&mut self, params: Params) {
        self.inner.reserve(params.len());

        let cap = params.iter().map(|(k, v)| k.len() + v.len()).sum();
        let mut buf = BytesMut::with_capacity(cap);

        for (k, v) in params.iter() {
            buf.put(k.as_bytes());
            // SAFETY: The key is a valid string
            let k = unsafe { FastStr::from_bytes_unchecked(buf.split().freeze()) };

            buf.put(v.as_bytes());
            // SAFETY: The value is a valid string
            let v = unsafe { FastStr::from_bytes_unchecked(buf.split().freeze()) };

            self.inner.push((k, v));
        }
    }
}

impl Deref for UrlParamsVec {
    type Target = [(FastStr, FastStr)];

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl FromContext for UrlParamsVec {
    type Rejection = Infallible;

    async fn from_context(cx: &mut ServerContext, _: &mut Parts) -> Result<Self, Self::Rejection> {
        Ok(cx.params().clone())
    }
}

#[derive(Debug, Default, Clone)]
pub struct UrlParamsMap {
    inner: AHashMap<FastStr, FastStr>,
}

impl Deref for UrlParamsMap {
    type Target = AHashMap<FastStr, FastStr>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl From<UrlParamsVec> for UrlParamsMap {
    fn from(value: UrlParamsVec) -> Self {
        let mut inner = AHashMap::with_capacity(value.inner.len());

        for (k, v) in value.inner.into_iter() {
            inner.insert(k, v);
        }

        Self { inner }
    }
}

impl FromContext for UrlParamsMap {
    type Rejection = Infallible;

    async fn from_context(cx: &mut ServerContext, _: &mut Parts) -> Result<Self, Self::Rejection> {
        let params = cx.params();
        let mut inner = AHashMap::with_capacity(params.len());

        for (k, v) in params.iter() {
            inner.insert(k.clone(), v.clone());
        }

        Ok(Self { inner })
    }
}

trait FromUrlParam: Sized {
    fn from_url_param(param: &str) -> Result<Self, UrlParamsRejection>;
}

macro_rules! impl_from_url_param {
    ($ty:ty) => {
        impl FromUrlParam for $ty {
            fn from_url_param(param: &str) -> Result<Self, UrlParamsRejection> {
                FromStr::from_str(param)
                    .map_err(Into::into)
                    .map_err(UrlParamsRejection::ParseError)
            }
        }
    };
}

impl_from_url_param!(bool);
impl_from_url_param!(u8);
impl_from_url_param!(u16);
impl_from_url_param!(u32);
impl_from_url_param!(u64);
impl_from_url_param!(usize);
impl_from_url_param!(i8);
impl_from_url_param!(i16);
impl_from_url_param!(i32);
impl_from_url_param!(i64);
impl_from_url_param!(isize);
impl_from_url_param!(char);
impl_from_url_param!(String);
impl_from_url_param!(FastStr);

#[derive(Debug, Default, Clone)]
pub struct UrlParams<T>(pub T);

impl<T> FromContext for UrlParams<T>
where
    T: FromUrlParam,
{
    type Rejection = UrlParamsRejection;

    async fn from_context(cx: &mut ServerContext, _: &mut Parts) -> Result<Self, Self::Rejection> {
        let mut param_iter = cx.params().iter();
        let t = T::from_url_param(
            param_iter
                .next()
                .ok_or(UrlParamsRejection::LengthMismatch)?
                .1
                .as_str(),
        )?;
        Ok(UrlParams(t))
    }
}

macro_rules! impl_url_params_extractor {
    (
        $($ty:ident),+ $(,)?
    ) => {
        #[allow(non_snake_case)]
        impl<$($ty,)+> FromContext for UrlParams<($($ty,)+)>
        where
            $(
                $ty: FromUrlParam,
            )+
        {
            type Rejection = UrlParamsRejection;

            async fn from_context(
                cx: &mut ServerContext,
                _: &mut Parts,
            ) -> Result<Self, Self::Rejection> {
                let mut param_iter = cx.params().iter();
                $(
                    let $ty = $ty::from_url_param(
                        param_iter.next().ok_or(UrlParamsRejection::LengthMismatch)?.1.as_str(),
                    )?;
                )+
                Ok(UrlParams(($($ty,)+)))
            }
        }
    };
}

all_the_tuples!(impl_url_params_extractor);

#[derive(Debug)]
pub enum UrlParamsRejection {
    LengthMismatch,
    ParseError(BoxError),
}

impl fmt::Display for UrlParamsRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LengthMismatch => write!(
                f,
                "the number of url params does not match number of types in `UrlParams`"
            ),
            Self::ParseError(e) => write!(f, "url param parse error: {e}"),
        }
    }
}

impl Error for UrlParamsRejection {}

impl IntoResponse for UrlParamsRejection {
    fn into_response(self) -> ServerResponse {
        StatusCode::BAD_REQUEST.into_response()
    }
}
