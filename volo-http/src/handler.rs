use std::{future::Future, marker::PhantomData};

use http::Response;
use hyper::body::Incoming;

use crate::{
    extract::FromContext,
    response::{IntoResponse, RespBody},
    HttpContext,
};

impl<H, T> Clone for HandlerService<H, T>
where
    H: Clone,
{
    fn clone(&self) -> Self {
        Self {
            h: self.h.clone(),
            _mark: PhantomData,
        }
    }
}
pub trait Handler<'r, T> {
    type Future: Future<Output = Response<RespBody>> + Send + 'r;
    fn call(self, context: &'r mut HttpContext) -> Self::Future;
}

impl<'r, F, Fut, T1, Res> Handler<'r, T1> for F
where
    F: FnOnce(T1) -> Fut + Clone + Send + 'r,
    Fut: Future<Output = Res> + Send + 'r,
    T1: FromContext + Send + 'r,
    Res: IntoResponse,
{
    type Future = impl Future<Output = Response<RespBody>> + Send + 'r;

    fn call(self, context: &'r mut HttpContext) -> Self::Future {
        async move {
            let t1 = match T1::from_context(context).await {
                Ok(value) => value,
                Err(rejection) => return rejection.into_response(),
            };
            self(t1).await.into_response()
        }
    }
}

impl<'r, F, Fut, T1, T2, Res> Handler<'r, (T1, T2)> for F
where
    F: FnOnce(T1, T2) -> Fut + Clone + Send + 'r,
    Fut: Future<Output = Res> + Send,
    T1: FromContext + Send + 'r,
    T2: FromContext + Send + 'r,
    Res: IntoResponse,
{
    type Future = impl Future<Output = Response<RespBody>> + Send + 'r;

    fn call(self, context: &'r mut HttpContext) -> Self::Future {
        async move {
            let t1 = match T1::from_context(context).await {
                Ok(value) => value,
                Err(rejection) => return rejection.into_response(),
            };
            let t2 = match T2::from_context(context).await {
                Ok(value) => value,
                Err(rejection) => return rejection.into_response(),
            };
            self(t1, t2).await.into_response()
        }
    }
}
pub struct HandlerService<H, T> {
    h: H,
    _mark: PhantomData<fn(T)>,
}

impl<H, T> HandlerService<H, T> {
    pub fn new(h: H) -> Self {
        Self {
            h,
            _mark: PhantomData,
        }
    }
}

impl<H, T> motore::Service<HttpContext, Incoming> for HandlerService<H, T>
where
    H: for<'r> Handler<'r, T> + Clone + Send + Sync,
{
    type Response = Response<RespBody>;
    type Error = http::Error;
    type Future<'cx> = impl Future<Output = Result<Self::Response, Self::Error>> + Send + 'cx
        where
            HttpContext: 'cx,
            Self: 'cx;

    fn call<'cx, 's>(&'s self, cx: &'cx mut HttpContext, _req: Incoming) -> Self::Future<'cx>
    where
        's: 'cx,
    {
        async move { Ok(self.h.clone().call(cx).await) }
    }
}
