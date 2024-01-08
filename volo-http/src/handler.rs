use std::{
    convert::Infallible,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use futures_util::future::BoxFuture;
use hyper::body::Incoming;
use motore::Service;

use crate::{
    extract::{FromContext, FromRequest},
    macros::{all_the_tuples, all_the_tuples_no_last_special_case},
    middleware::Next,
    response::{IntoResponse, Response},
    DynService, HttpContext,
};

pub trait Handler<T, S>: Sized {
    fn call(
        self,
        cx: &mut HttpContext,
        req: Incoming,
        state: &S,
    ) -> impl Future<Output = Response> + Send;

    fn with_state(self, state: S) -> HandlerService<Self, S, T>
    where
        S: Clone,
    {
        HandlerService {
            handler: self,
            state,
            _marker: PhantomData,
        }
    }
}

impl<F, Fut, Res, S> Handler<((),), S> for F
where
    F: FnOnce() -> Fut + Clone + Send,
    Fut: Future<Output = Res> + Send,
    Res: IntoResponse,
    S: Send + Sync,
{
    async fn call(self, _context: &mut HttpContext, _req: Incoming, _state: &S) -> Response {
        self().await.into_response()
    }
}

macro_rules! impl_handler {
    (
        [$($ty:ident),*], $last:ident
    ) => {
        #[allow(non_snake_case, unused_mut, unused_variables)]
        impl<F, Fut, Res, M, S, $($ty,)* $last> Handler<(M, $($ty,)* $last,), S> for F
        where
            F: FnOnce($($ty,)* $last) -> Fut + Clone + Send,
            Fut: Future<Output = Res> + Send,
            Res: IntoResponse,
            S: Send + Sync,
            $( for<'r> $ty: FromContext<S> + Send + 'r, )*
            for<'r> $last: FromRequest<S, M> + Send + 'r,
        {
            async fn call(self, cx: &mut HttpContext, req: Incoming, state: &S) -> Response {
                $(
                    let $ty = match $ty::from_context(cx, state).await {
                        Ok(value) => value,
                        Err(rejection) => return rejection.into_response(),
                    };
                )*
                let $last = match $last::from_request(cx, req, state).await {
                    Ok(value) => value,
                    Err(rejection) => return rejection.into_response(),
                };
                self($($ty,)* $last).await.into_response()
            }
        }
    };
}

all_the_tuples!(impl_handler);

// Use an extra trait with less generic types for hiding the type of handler
pub struct DynHandler<S>(Box<dyn ErasedIntoRoute<S>>);

unsafe impl<S> Send for DynHandler<S> {}
unsafe impl<S> Sync for DynHandler<S> {}

impl<S> DynHandler<S>
where
    S: Clone + Send + Sync + 'static,
{
    pub(crate) fn new<H, T>(handler: H) -> Self
    where
        H: Handler<T, S> + Clone + Send + Sync + 'static,
        T: 'static,
    {
        // The anonymous function should ensure that the `handler` must be an impl of `Handler`,
        // but the `ErasedIntoRoute::into_route` does not need to care it.
        Self(Box::new(MakeErasedHandler {
            handler,
            into_route: |handler, state| {
                DynService::new(HandlerService {
                    handler,
                    state,
                    _marker: PhantomData,
                })
            },
        }))
    }

    // State can only be injected into handler because route does not have such a field, so before
    // injected a state, a handler should keep being a handler.
    pub(crate) fn map<F>(self, f: F) -> DynHandler<S>
    where
        F: FnOnce(DynService) -> DynService + Clone + 'static,
    {
        DynHandler(Box::new(LayerMap {
            inner: self.0,
            layer: Box::new(f),
        }))
    }

    pub(crate) fn into_route(self, state: S) -> DynService {
        self.0.into_route(state)
    }

    pub(crate) async fn call_with_state(
        self,
        cx: &mut HttpContext,
        req: Incoming,
        state: S,
    ) -> Result<Response, Infallible> {
        self.0.into_route(state).call(cx, req).await
    }
}

impl<S> Clone for DynHandler<S> {
    fn clone(&self) -> Self {
        Self(self.0.clone_box())
    }
}

pub(crate) trait ErasedIntoRoute<S> {
    fn clone_box(&self) -> Box<dyn ErasedIntoRoute<S>>;
    fn into_route(self: Box<Self>, state: S) -> DynService;
}

pub(crate) struct MakeErasedHandler<H, S> {
    handler: H,
    into_route: fn(H, S) -> DynService,
}

impl<H, S> ErasedIntoRoute<S> for MakeErasedHandler<H, S>
where
    H: Clone + 'static,
    S: 'static,
{
    fn clone_box(&self) -> Box<dyn ErasedIntoRoute<S>> {
        Box::new(self.clone())
    }

    fn into_route(self: Box<Self>, state: S) -> DynService {
        motore::BoxCloneService::new((self.into_route)(self.handler, state))
    }
}

impl<H, S> Clone for MakeErasedHandler<H, S>
where
    H: Clone,
{
    fn clone(&self) -> Self {
        Self {
            handler: self.handler.clone(),
            into_route: self.into_route,
        }
    }
}

struct LayerMap<S> {
    inner: Box<dyn ErasedIntoRoute<S>>,
    layer: Box<dyn LayerFn>,
}

trait LayerFn: FnOnce(DynService) -> DynService {
    fn clone_box(&self) -> Box<dyn LayerFn>;
}

impl<F> LayerFn for F
where
    F: FnOnce(DynService) -> DynService + Clone + 'static,
{
    fn clone_box(&self) -> Box<dyn LayerFn> {
        Box::new(self.clone())
    }
}

impl<S> ErasedIntoRoute<S> for LayerMap<S>
where
    S: 'static,
{
    fn clone_box(&self) -> Box<dyn ErasedIntoRoute<S>> {
        Box::new(Self {
            inner: self.inner.clone_box(),
            layer: self.layer.clone_box(),
        })
    }

    fn into_route(self: Box<Self>, state: S) -> DynService {
        (self.layer)(self.inner.into_route(state))
    }
}

pub struct HandlerService<H, S, T> {
    handler: H,
    state: S,
    _marker: PhantomData<fn(T)>,
}

impl<H, S, T> Clone for HandlerService<H, S, T>
where
    H: Clone,
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            handler: self.handler.clone(),
            state: self.state.clone(),
            _marker: PhantomData,
        }
    }
}

impl<H, S, T> motore::Service<HttpContext, Incoming> for HandlerService<H, S, T>
where
    for<'r> H: Handler<T, S> + Clone + Send + Sync + 'r,
    S: Sync,
{
    type Response = Response;
    type Error = Infallible;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut HttpContext,
        req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        Ok(self.handler.clone().call(cx, req, &self.state).await)
    }
}

pub trait HandlerWithoutRequest<T, Ret>: Sized {
    fn call(self, cx: &mut HttpContext) -> impl Future<Output = Result<Ret, Response>> + Send;
}

impl<F, Fut, Ret> HandlerWithoutRequest<(), Ret> for F
where
    F: FnOnce() -> Fut + Clone + Send,
    Fut: Future<Output = Ret> + Send,
{
    async fn call(self, _context: &mut HttpContext) -> Result<Ret, Response> {
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
            $( for<'r> $ty: FromContext<()> + Send + 'r, )*
        {
            async fn call(self, cx: &mut HttpContext) -> Result<Ret, Response> {
                $(
                    let $ty = match $ty::from_context(cx, &()).await {
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

pub trait MiddlewareHandlerFromFn<'r, T, S>: Sized {
    // type Response: IntoResponse;
    type Future: Future<Output = Response> + Send + 'r;

    fn call(
        &self,
        cx: &'r mut HttpContext,
        req: Incoming,
        state: &'r S,
        next: Next,
    ) -> Self::Future;
}

macro_rules! impl_middleware_handler_from_fn {
    (
        [$($ty:ident),*], $last:ident
    ) => {
        #[allow(non_snake_case, unused_mut, unused_variables)]
        impl<'r, F, Fut, Res, M, S, $($ty,)* $last> MiddlewareHandlerFromFn<'r, (M, $($ty,)* $last), S> for F
        where
            F: Fn($($ty,)* &'r mut HttpContext, $last, Next) -> Fut + Copy + Send + Sync + 'static,
            Fut: Future<Output = Res> + Send + 'r,
            Res: IntoResponse + 'r,
            S: Send + Sync + 'r,
            $( $ty: FromContext<S> + Send + 'r, )*
            $last: FromRequest<S, M> + Send + 'r,
        {
            // type Response = Response;
            type Future = ResponseFuture<'r, Response>;

            fn call(
                &self,
                cx: &'r mut HttpContext,
                req: Incoming,
                state: &'r S,
                next: Next,
            ) -> Self::Future {
                let f = *self;

                let future = Box::pin(async move {
                    $(
                        let $ty = match $ty::from_context(cx, state).await {
                            Ok(value) => value,
                            Err(rejection) => return rejection.into_response(),
                        };
                    )*
                    let $last = match $last::from_request(cx, req, state).await {
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

pub trait MiddlewareHandlerMapResponse<'r, T, S>: Sized {
    // type Response: IntoResponse;
    type Future: Future<Output = Response> + Send + 'r;

    fn call(&self, cx: &'r mut HttpContext, state: &'r S, response: Response) -> Self::Future;
}

impl<'r, F, Fut, Res, S> MiddlewareHandlerMapResponse<'r, ((),), S> for F
where
    F: Fn(Response) -> Fut + Copy + Send + Sync + 'static,
    Fut: Future<Output = Res> + Send + 'r,
    Res: IntoResponse + 'r,
    S: Send + Sync + 'r,
{
    // type Response = Response;
    type Future = ResponseFuture<'r, Response>;

    fn call(
        &self,
        _context: &'r mut HttpContext,
        _state: &'r S,
        response: Response,
    ) -> Self::Future {
        let f = *self;

        let future = Box::pin(async move { f(response).await.into_response() });

        ResponseFuture { inner: future }
    }
}

macro_rules! impl_middleware_handler_map_response {
    (
        $($ty:ident),* $(,)?
    ) => {
        #[allow(non_snake_case, unused_mut, unused_variables)]
        impl<'r, F, Fut, Res, M, S, $($ty,)*> MiddlewareHandlerMapResponse<'r, (M, $($ty,)*), S> for F
        where
            F: Fn($($ty,)* Response) -> Fut + Copy + Send + Sync + 'static,
            Fut: Future<Output = Res> + Send + 'r,
            Res: IntoResponse + 'r,
            S: Send + Sync + 'r,
            $( $ty: FromContext<S> + Send + 'r, )*
        {
            // type Response = Response;
            type Future = ResponseFuture<'r, Response>;

            fn call(
                &self,
                cx: &'r mut HttpContext,
                state: &'r S,
                response: Response,
            ) -> Self::Future {
                let f = *self;

                let future = Box::pin(async move {
                    $(
                        let $ty = match $ty::from_context(cx, state).await {
                            Ok(value) => value,
                            Err(rejection) => return rejection.into_response(),
                        };
                    )*
                    f($($ty,)* response).await.into_response()
                });

                ResponseFuture {
                    inner: future,
                }
            }
        }
    };
}

all_the_tuples_no_last_special_case!(impl_middleware_handler_map_response);

/// Response future for [`MapResponse`].
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
