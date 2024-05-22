use std::{
    future::Future,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use futures_util::future::BoxFuture;
use http::request::Parts;
use http_body::Body;
use motore::service::Service;

use super::{
    extract::{FromContext, FromRequest},
    middleware::Next,
    IntoResponse,
};
use crate::{
    context::ServerContext,
    request::ServerRequest,
    response::ServerResponse,
    utils::macros::{all_the_tuples, all_the_tuples_with_special_case},
};

pub trait Handler<T, B, E>: Sized {
    fn handle(
        self,
        cx: &mut ServerContext,
        req: ServerRequest<B>,
    ) -> impl Future<Output = ServerResponse> + Send;

    fn into_service(self) -> HandlerService<Self, T, B, E> {
        HandlerService {
            handler: self,
            _marker: PhantomData,
        }
    }
}

impl<F, Fut, Res, B, E> Handler<((),), B, E> for F
where
    F: FnOnce() -> Fut + Clone + Send,
    Fut: Future<Output = Res> + Send,
    Res: IntoResponse,
    B: Send,
{
    async fn handle(self, _cx: &mut ServerContext, _: ServerRequest<B>) -> ServerResponse {
        self().await.into_response()
    }
}

macro_rules! impl_handler {
    (
        [$($ty:ident),*], $last:ident
    ) => {
        #[allow(non_snake_case, unused_mut, unused_variables)]
        impl<F, Fut, Res, M, $($ty,)* $last, B, E> Handler<(M, $($ty,)* $last,), B, E> for F
        where
            F: FnOnce($($ty,)* $last) -> Fut + Clone + Send,
            Fut: Future<Output = Res> + Send,
            Res: IntoResponse,
            $( for<'r> $ty: FromContext + Send + 'r, )*
            for<'r> $last: FromRequest<B, M> + Send + 'r,
            B: Body + Send,
            B::Data: Send,
            B::Error: Send,
        {
            async fn handle(self, cx: &mut ServerContext, req: ServerRequest<B>) -> ServerResponse {
                let (mut parts, body) = req.into_parts();
                $(
                    let $ty = match $ty::from_context(cx, &mut parts).await {
                        Ok(value) => value,
                        Err(rejection) => return rejection.into_response(),
                    };
                )*
                let $last = match $last::from_request(cx, parts, body).await {
                    Ok(value) => value,
                    Err(rejection) => return rejection.into_response(),
                };
                self($($ty,)* $last).await.into_response()
            }
        }
    };
}

all_the_tuples_with_special_case!(impl_handler);

pub struct HandlerService<H, T, B, E> {
    handler: H,
    _marker: PhantomData<fn(T, B, E)>,
}

impl<H, T, B, E> Clone for HandlerService<H, T, B, E>
where
    H: Clone,
{
    fn clone(&self) -> Self {
        Self {
            handler: self.handler.clone(),
            _marker: self._marker,
        }
    }
}

impl<H, T, B, E> Service<ServerContext, ServerRequest<B>> for HandlerService<H, T, B, E>
where
    H: Handler<T, B, E> + Clone + Send + Sync,
    B: Send,
{
    type Response = ServerResponse;
    type Error = E;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        Ok(self.handler.clone().handle(cx, req).await)
    }
}

pub trait HandlerWithoutRequest<T, Ret>: Sized {
    fn handle(
        self,
        cx: &mut ServerContext,
        parts: &mut Parts,
    ) -> impl Future<Output = Result<Ret, ServerResponse>> + Send;
}

impl<F, Fut, Ret> HandlerWithoutRequest<(), Ret> for F
where
    F: FnOnce() -> Fut + Clone + Send,
    Fut: Future<Output = Ret> + Send,
{
    async fn handle(self, _: &mut ServerContext, _: &mut Parts) -> Result<Ret, ServerResponse> {
        Ok(self().await)
    }
}

macro_rules! impl_handler_without_request {
    (
        $($ty:ident),* $(,)?
    ) => {
        #[allow(non_snake_case, unused_mut, unused_variables)]
        impl<F, Fut, Ret, $($ty,)*> HandlerWithoutRequest<($($ty,)*), Ret> for F
        where
            F: FnOnce($($ty,)*) -> Fut + Clone + Send,
            Fut: Future<Output = Ret> + Send,
            $( for<'r> $ty: FromContext + Send + 'r, )*
        {
            async fn handle(
                self,
                cx: &mut ServerContext,
                parts: &mut Parts,
            ) -> Result<Ret, ServerResponse> {
                $(
                    let $ty = match $ty::from_context(cx, parts).await {
                        Ok(value) => value,
                        Err(rejection) => return Err(rejection.into_response()),
                    };
                )*
                Ok(self($($ty,)*).await)
            }
        }
    };
}

all_the_tuples!(impl_handler_without_request);

pub trait MiddlewareHandlerFromFn<'r, T, B, B2, E2>: Sized {
    type Future: Future<Output = ServerResponse> + Send + 'r;

    fn handle(
        &self,
        cx: &'r mut ServerContext,
        req: ServerRequest<B>,
        next: Next<B2, E2>,
    ) -> Self::Future;
}

impl<'r, F, Fut, Res, B, B2, E2> MiddlewareHandlerFromFn<'r, (), B, B2, E2> for F
where
    F: Fn(&'r mut ServerContext, ServerRequest<B>, Next<B2, E2>) -> Fut + Copy + Send + Sync + 'r,
    Fut: Future<Output = Res> + Send + 'r,
    Res: IntoResponse + 'r,
    B: Send + 'r,
    B2: Send + 'r,
    E2: 'r,
{
    type Future = ResponseFuture<'r, ServerResponse>;

    fn handle(
        &self,
        cx: &'r mut ServerContext,
        req: ServerRequest<B>,
        next: Next<B2, E2>,
    ) -> Self::Future {
        let f = *self;

        let future = Box::pin(async move { f(cx, req, next).await.into_response() });

        ResponseFuture { inner: future }
    }
}

macro_rules! impl_middleware_handler_from_fn {
    (
        $($ty:ident),* $(,)?
    ) => {
        #[allow(non_snake_case, unused_mut, unused_variables)]
        impl<'r, F, Fut, Res, $($ty,)* B, B2, E2>
            MiddlewareHandlerFromFn<'r, ($($ty,)*), B, B2, E2> for F
        where
            F: Fn($($ty,)* &'r mut ServerContext, ServerRequest<B>, Next<B2, E2>) -> Fut
                + Copy
                + Send
                + Sync
                + 'r,
            Fut: Future<Output = Res> + Send + 'r,
            Res: IntoResponse + 'r,
            $( $ty: FromContext + Send + 'r, )*
            B: Send + 'r,
            B2: Send + 'r,
            E2: 'r,
        {
            type Future = ResponseFuture<'r, ServerResponse>;

            fn handle(
                &self,
                cx: &'r mut ServerContext,
                req: ServerRequest<B>,
                next: Next<B2, E2>,
            ) -> Self::Future {
                let f = *self;

                let future = Box::pin(async move {
                    let (mut parts, body) = req.into_parts();
                    $(
                        let $ty = match $ty::from_context(cx, &mut parts).await {
                            Ok(value) => value,
                            Err(rejection) => return rejection.into_response(),
                        };
                    )*
                    let req = ServerRequest::from_parts(parts, body);
                    f($($ty,)* cx, req, next).await.into_response()
                });

                ResponseFuture {
                    inner: future,
                }
            }
        }
    };
}

all_the_tuples!(impl_middleware_handler_from_fn);

pub trait MiddlewareHandlerMapResponse<'r, T, R1, R2>: Sized {
    type Future: Future<Output = R2> + Send + 'r;
    fn handle(&self, cx: &'r mut ServerContext, resp: R1) -> Self::Future;
}

impl<'r, F, Fut, R1, R2> MiddlewareHandlerMapResponse<'r, ((),), R1, R2> for F
where
    F: Fn(R1) -> Fut + Copy + Send + Sync + 'r,
    Fut: Future<Output = R2> + Send + 'r,
    R2: 'r,
{
    type Future = ResponseFuture<'r, R2>;

    fn handle(&self, _: &'r mut ServerContext, resp: R1) -> Self::Future {
        let f = *self;

        ResponseFuture {
            inner: Box::pin(f(resp)),
        }
    }
}

impl<'r, F, Fut, R1, R2> MiddlewareHandlerMapResponse<'r, (ServerContext,), R1, R2> for F
where
    F: Fn(&mut ServerContext, R1) -> Fut + Copy + Send + Sync + 'r,
    Fut: Future<Output = R2> + Send + 'r,
    R2: 'r,
{
    type Future = ResponseFuture<'r, R2>;

    fn handle(&self, cx: &'r mut ServerContext, resp: R1) -> Self::Future {
        let f = *self;

        ResponseFuture {
            inner: Box::pin(f(cx, resp)),
        }
    }
}

pub struct ResponseFuture<'r, Res> {
    inner: BoxFuture<'r, Res>,
}

impl<'r, Res> Future for ResponseFuture<'r, Res>
where
    Res: 'r,
{
    type Output = Res;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.inner.as_mut().poll(cx)
    }
}
