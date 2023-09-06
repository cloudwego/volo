use std::{future::Future, marker::PhantomData};

use http::Response;
use hyper::body::Incoming;

use crate::{
    extract::FromContext,
    request::FromRequest,
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
pub trait Handler<T> {
    type Future<'r>: Future<Output = Response<RespBody>> + Send + 'r
    where
        Self: 'r;
    fn call(self, context: &mut HttpContext, req: Incoming) -> Self::Future<'_>;
}

macro_rules! impl_handler {
    (
        [$($ty:ident),*], $last:ident
    ) => {
        #[allow(non_snake_case, unused_mut, unused_variables)]
        impl<F, Fut, $($ty,)* $last, Res> Handler<($($ty,)* $last,)> for F
        where
            F: FnOnce($($ty,)* $last) -> Fut + Clone + Send,
            Fut: Future<Output = Res> + Send,
            $( for<'r> $ty: FromContext + Send + 'r, )*
            for<'r> $last: FromRequest + Send + 'r,
            Res: IntoResponse,
        {
            type Future<'r> = impl Future<Output=Response<RespBody>> + Send + 'r
                where Self: 'r;

            fn call(self, context: &mut HttpContext, req: Incoming) -> Self::Future<'_> {
                async move {
                    $(
                        let $ty = match $ty::from_context(context).await {
                            Ok(value) => value,
                            Err(rejection) => return rejection.into_response(),
                        };
                    )*
                    let $last = match $last::from(context, req).await {
                        Ok(value) => value,
                        Err(rejection) => return rejection,
                    };
                    self($($ty,)* $last).await.into_response()
                }
            }
        }
    };
}

impl_handler!([], T1);
impl_handler!([T1], T2);
impl_handler!([T1, T2], T3);
impl_handler!([T1, T2, T3], T4);
impl_handler!([T1, T2, T3, T4], T5);
impl_handler!([T1, T2, T3, T4, T5], T6);
impl_handler!([T1, T2, T3, T4, T5, T6], T7);
impl_handler!([T1, T2, T3, T4, T5, T6, T7], T8);
impl_handler!([T1, T2, T3, T4, T5, T6, T7, T8], T9);
impl_handler!([T1, T2, T3, T4, T5, T6, T7, T8, T9], T10);
impl_handler!([T1, T2, T3, T4, T5, T6, T7, T8, T9, T10], T11);
impl_handler!([T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11], T12);
impl_handler!([T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12], T13);
impl_handler!(
    [T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13],
    T14
);
impl_handler!(
    [T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14],
    T15
);
impl_handler!(
    [T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15],
    T16
);

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
    for<'r> H: Handler<T> + Clone + Send + Sync + 'r,
{
    type Response = Response<RespBody>;
    type Error = http::Error;
    type Future<'cx> = impl Future<Output = Result<Self::Response, Self::Error>> + Send + 'cx
        where
            HttpContext: 'cx,
            Self: 'cx;

    fn call<'cx, 's>(&'s self, cx: &'cx mut HttpContext, req: Incoming) -> Self::Future<'cx>
    where
        's: 'cx,
    {
        async move { Ok(self.h.clone().call(cx, req).await) }
    }
}
