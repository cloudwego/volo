use std::{future::Future, marker::PhantomData};

use http::Response;
use hyper::body::Incoming;
use motore::Service;

use crate::{
    extract::FromContext,
    request::FromRequest,
    response::{IntoResponse, RespBody},
    DynError, DynService, HttpContext,
};

pub trait Handler<T, S>: Sized {
    fn call(
        self,
        context: &mut HttpContext,
        req: Incoming,
        state: &S,
    ) -> impl Future<Output = Response<RespBody>> + Send;

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
    async fn call(
        self,
        _context: &mut HttpContext,
        _req: Incoming,
        _state: &S,
    ) -> Response<RespBody> {
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
            async fn call(self, context: &mut HttpContext, req: Incoming, state: &S) -> Response<RespBody> {
                $(
                    let $ty = match $ty::from_context(context, state).await {
                        Ok(value) => value,
                        Err(rejection) => return rejection.into_response(),
                    };
                )*
                let $last = match $last::from(context, req, state).await {
                    Ok(value) => value,
                    Err(rejection) => return rejection,
                };
                self($($ty,)* $last).await.into_response()
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
    ) -> Result<Response<RespBody>, DynError> {
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
    type Response = Response<RespBody>;
    type Error = DynError;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut HttpContext,
        req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        Ok(self.handler.clone().call(cx, req, &self.state).await)
    }
}
