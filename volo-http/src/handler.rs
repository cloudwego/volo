use std::{
    convert::Infallible,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use futures_util::future::BoxFuture;
use http::request::Parts;
use motore::service::Service;

use crate::{
    context::ServerContext,
    extract::{FromContext, FromRequest},
    macros::{all_the_tuples, all_the_tuples_no_last_special_case},
    middleware::Next,
    request::ServerRequest,
    response::{IntoResponse, ServerResponse},
};

pub trait Handler<T>: Sized {
    fn handle(
        self,
        cx: &mut ServerContext,
        req: ServerRequest,
    ) -> impl Future<Output = ServerResponse> + Send;

    fn into_service(self) -> HandlerService<Self, T> {
        HandlerService {
            handler: self,
            _marker: PhantomData,
        }
    }
}

impl<F, Fut, Res> Handler<((),)> for F
where
    F: FnOnce() -> Fut + Clone + Send,
    Fut: Future<Output = Res> + Send,
    Res: IntoResponse,
{
    async fn handle(self, _cx: &mut ServerContext, _req: ServerRequest) -> ServerResponse {
        self().await.into_response()
    }
}

macro_rules! impl_handler {
    (
        [$($ty:ident),*], $last:ident
    ) => {
        #[allow(non_snake_case, unused_mut, unused_variables)]
        impl<F, Fut, Res, M, $($ty,)* $last> Handler<(M, $($ty,)* $last,)> for F
        where
            F: FnOnce($($ty,)* $last) -> Fut + Clone + Send,
            Fut: Future<Output = Res> + Send,
            Res: IntoResponse,
            $( for<'r> $ty: FromContext + Send + 'r, )*
            for<'r> $last: FromRequest<M> + Send + 'r,
        {
            async fn handle(self, cx: &mut ServerContext, req: ServerRequest) -> ServerResponse {
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

all_the_tuples!(impl_handler);

pub struct HandlerService<H, T> {
    handler: H,
    _marker: PhantomData<fn(T)>,
}

impl<H, T> Clone for HandlerService<H, T>
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

impl<H, T> Service<ServerContext, ServerRequest> for HandlerService<H, T>
where
    H: Handler<T> + Clone + Send + Sync,
{
    type Response = ServerResponse;
    type Error = Infallible;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest,
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
    async fn handle(
        self,
        _context: &mut ServerContext,
        _parts: &mut Parts,
    ) -> Result<Ret, ServerResponse> {
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
            async fn handle(self, cx: &mut ServerContext, parts: &mut Parts) -> Result<Ret, ServerResponse> {
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

all_the_tuples_no_last_special_case!(impl_handler_without_request);

pub trait MiddlewareHandlerFromFn<'r, T>: Sized {
    type Future: Future<Output = ServerResponse> + Send + 'r;

    fn handle(&self, cx: &'r mut ServerContext, req: ServerRequest, next: Next) -> Self::Future;
}

macro_rules! impl_middleware_handler_from_fn {
    (
        [$($ty:ident),*], $last:ident
    ) => {
        #[allow(non_snake_case, unused_mut, unused_variables)]
        impl<'r, F, Fut, Res, M, $($ty,)* $last> MiddlewareHandlerFromFn<'r, (M, $($ty,)* $last)> for F
        where
            F: Fn($($ty,)* &'r mut ServerContext, $last, Next) -> Fut + Copy + Send + Sync + 'static,
            Fut: Future<Output = Res> + Send + 'r,
            Res: IntoResponse + 'r,
            $( $ty: FromContext + Send + 'r, )*
            $last: FromRequest<M> + Send + 'r,
        {
            type Future = ResponseFuture<'r, ServerResponse>;

            fn handle(
                &self,
                cx: &'r mut ServerContext,
                req: ServerRequest,
                next: Next,
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
                    let $last = match $last::from_request(cx, parts, body).await {
                        Ok(value) => value,
                        Err(rejection) => return rejection.into_response(),
                    };
                    f($($ty,)* cx, $last, next).await.into_response()
                });

                ResponseFuture {
                    inner: future,
                }
            }
        }
    };
}

all_the_tuples!(impl_middleware_handler_from_fn);

pub trait MiddlewareHandlerMapResponse<'r, T>: Sized {
    type Future: Future<Output = ServerResponse> + Send + 'r;

    fn handle(&self, cx: &'r mut ServerContext, response: ServerResponse) -> Self::Future;
}

impl<'r, F, Fut, Res> MiddlewareHandlerMapResponse<'r, ((),)> for F
where
    F: Fn(ServerResponse) -> Fut + Copy + Send + Sync + 'static,
    Fut: Future<Output = Res> + Send + 'r,
    Res: IntoResponse + 'r,
{
    type Future = ResponseFuture<'r, ServerResponse>;

    fn handle(&self, _context: &'r mut ServerContext, response: ServerResponse) -> Self::Future {
        let f = *self;

        let future = Box::pin(async move { f(response).await.into_response() });

        ResponseFuture { inner: future }
    }
}

impl<'r, F, Fut, Res> MiddlewareHandlerMapResponse<'r, (ServerContext,)> for F
where
    F: Fn(&mut ServerContext, ServerResponse) -> Fut + Copy + Send + Sync + 'static,
    Fut: Future<Output = Res> + Send + 'r,
    Res: IntoResponse + 'r,
{
    type Future = ResponseFuture<'r, ServerResponse>;

    fn handle(&self, context: &'r mut ServerContext, response: ServerResponse) -> Self::Future {
        let f = *self;

        let future = Box::pin(async move { f(context, response).await.into_response() });

        ResponseFuture { inner: future }
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
