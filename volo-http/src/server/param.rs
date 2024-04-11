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

pub type UrlParamsVec = PathParamsVec;
pub type UrlParamsMap = PathParamsMap;
pub type UrlParams<T> = PathParams<T>;
pub type UrlParamsRejection = PathParamsRejection;

#[derive(Clone, Debug, Default)]
pub struct PathParamsVec {
    inner: Vec<(FastStr, FastStr)>,
}

impl PathParamsVec {
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

impl Deref for PathParamsVec {
    type Target = [(FastStr, FastStr)];

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl FromContext for PathParamsVec {
    type Rejection = Infallible;

    async fn from_context(cx: &mut ServerContext, _: &mut Parts) -> Result<Self, Self::Rejection> {
        Ok(cx.params().clone())
    }
}

#[derive(Debug, Default, Clone)]
pub struct PathParamsMap {
    inner: AHashMap<FastStr, FastStr>,
}

impl Deref for PathParamsMap {
    type Target = AHashMap<FastStr, FastStr>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl From<PathParamsVec> for PathParamsMap {
    fn from(value: PathParamsVec) -> Self {
        let mut inner = AHashMap::with_capacity(value.inner.len());

        for (k, v) in value.inner.into_iter() {
            inner.insert(k, v);
        }

        Self { inner }
    }
}

impl FromContext for PathParamsMap {
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

trait FromPathParam: Sized {
    fn from_path_param(param: &str) -> Result<Self, PathParamsRejection>;
}

macro_rules! impl_from_path_param {
    ($ty:ty) => {
        impl FromPathParam for $ty {
            fn from_path_param(param: &str) -> Result<Self, PathParamsRejection> {
                FromStr::from_str(param)
                    .map_err(Into::into)
                    .map_err(PathParamsRejection::ParseError)
            }
        }
    };
}

impl_from_path_param!(bool);
impl_from_path_param!(u8);
impl_from_path_param!(u16);
impl_from_path_param!(u32);
impl_from_path_param!(u64);
impl_from_path_param!(usize);
impl_from_path_param!(i8);
impl_from_path_param!(i16);
impl_from_path_param!(i32);
impl_from_path_param!(i64);
impl_from_path_param!(isize);
impl_from_path_param!(char);
impl_from_path_param!(String);
impl_from_path_param!(FastStr);

#[derive(Debug, Default, Clone)]
pub struct PathParams<T>(pub T);

impl<T> FromContext for PathParams<T>
where
    T: FromPathParam,
{
    type Rejection = PathParamsRejection;

    async fn from_context(cx: &mut ServerContext, _: &mut Parts) -> Result<Self, Self::Rejection> {
        let mut param_iter = cx.params().iter();
        let t = T::from_path_param(
            param_iter
                .next()
                .ok_or(PathParamsRejection::LengthMismatch)?
                .1
                .as_str(),
        )?;
        Ok(PathParams(t))
    }
}

macro_rules! impl_path_params_extractor {
    (
        $($ty:ident),+ $(,)?
    ) => {
        #[allow(non_snake_case)]
        impl<$($ty,)+> FromContext for PathParams<($($ty,)+)>
        where
            $(
                $ty: FromPathParam,
            )+
        {
            type Rejection = PathParamsRejection;

            async fn from_context(
                cx: &mut ServerContext,
                _: &mut Parts,
            ) -> Result<Self, Self::Rejection> {
                let mut param_iter = cx.params().iter();
                $(
                    let $ty = $ty::from_path_param(
                        param_iter.next().ok_or(PathParamsRejection::LengthMismatch)?.1.as_str(),
                    )?;
                )+
                Ok(PathParams(($($ty,)+)))
            }
        }
    };
}

all_the_tuples!(impl_path_params_extractor);

#[derive(Debug)]
pub enum PathParamsRejection {
    LengthMismatch,
    ParseError(BoxError),
}

impl fmt::Display for PathParamsRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LengthMismatch => write!(
                f,
                "the number of path params does not match number of types in `PathParams`"
            ),
            Self::ParseError(e) => write!(f, "path param parse error: {e}"),
        }
    }
}

impl Error for PathParamsRejection {}

impl IntoResponse for PathParamsRejection {
    fn into_response(self) -> ServerResponse {
        StatusCode::BAD_REQUEST.into_response()
    }
}
