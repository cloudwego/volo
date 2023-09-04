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
    fn call(self, context: &'r mut HttpContext, req: Incoming) -> Self::Future;
}

macro_rules! impl_handler {
    (
        [$($ty:ident),*],
    ) => {
        #[allow(non_snake_case, unused_mut)]
        impl<'r, F, Fut, $($ty,)* Res> Handler<'r, ($($ty,)*)> for F
        where
            F: FnOnce($($ty,)*) -> Fut + Clone + Send + 'r,
            Fut: Future<Output = Res> + Send,
            $( $ty: FromContext + Send + 'r, )*
            Res: IntoResponse,
        {
            type Future = impl Future<Output = Response<RespBody>> + Send + 'r;

            fn call(self, context: &'r mut HttpContext, _req: Incoming) -> Self::Future {
                async move {
                    $(
                        let $ty = match $ty::from_context(context).await {
                            Ok(value) => value,
                            Err(rejection) => return rejection.into_response(),
                        };
                    )*
                    self($($ty,)*).await.into_response()
                }
            }
        }
    };
}

impl_handler!([],);
impl_handler!([T1],);
impl_handler!([T1, T2],);
impl_handler!([T1, T2, T3],);
impl_handler!([T1, T2, T3, T4],);
impl_handler!([T1, T2, T3, T4, T5],);
impl_handler!([T1, T2, T3, T4, T5, T6],);
impl_handler!([T1, T2, T3, T4, T5, T6, Y7],);
impl_handler!([T1, T2, T3, T4, T5, T6, Y7, T8],);
impl_handler!([T1, T2, T3, T4, T5, T6, Y7, T8, T9],);
impl_handler!([T1, T2, T3, T4, T5, T6, Y7, T8, T9, T10],);
impl_handler!([T1, T2, T3, T4, T5, T6, Y7, T8, T9, T10, T11],);
impl_handler!([T1, T2, T3, T4, T5, T6, Y7, T8, T9, T10, T11, T12],);
impl_handler!([T1, T2, T3, T4, T5, T6, Y7, T8, T9, T10, T11, T12, T13],);
impl_handler!([T1, T2, T3, T4, T5, T6, Y7, T8, T9, T10, T11, T12, T13, T14],);
impl_handler!([T1, T2, T3, T4, T5, T6, Y7, T8, T9, T10, T11, T12, T13, T14, T15],);

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

    fn call<'cx, 's>(&'s self, cx: &'cx mut HttpContext, req: Incoming) -> Self::Future<'cx>
    where
        's: 'cx,
    {
        async move { Ok(self.h.clone().call(cx, req).await) }
    }
}
