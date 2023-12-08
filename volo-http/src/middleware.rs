use std::{convert::Infallible, marker::PhantomData};

use hyper::body::Incoming;
use motore::{layer::Layer, service::Service};

use crate::{handler::MiddlewareHandlerFromFn, response::Response, DynService, HttpContext};

pub struct FromFnLayer<F, S, T> {
    f: F,
    state: S,
    _marker: PhantomData<fn(T)>,
}

impl<F, S, T> Clone for FromFnLayer<F, S, T>
where
    F: Clone,
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            f: self.f.clone(),
            state: self.state.clone(),
            _marker: PhantomData,
        }
    }
}

pub fn from_fn<F, T>(f: F) -> FromFnLayer<F, (), T> {
    from_fn_with_state(f, ())
}

fn from_fn_with_state<F, S, T>(f: F, state: S) -> FromFnLayer<F, S, T> {
    FromFnLayer {
        f,
        state,
        _marker: PhantomData,
    }
}

impl<I, F, S, T> Layer<I> for FromFnLayer<F, S, T>
where
    F: Clone,
    S: Clone,
{
    type Service = FromFn<I, F, S, T>;

    fn layer(self, inner: I) -> Self::Service {
        FromFn {
            inner,
            f: self.f.clone(),
            state: self.state.clone(),
            _marker: self._marker,
        }
    }
}

pub struct FromFn<I, F, S, T> {
    inner: I,
    f: F,
    state: S,
    _marker: PhantomData<fn(T)>,
}

impl<I, F, S, T> Clone for FromFn<I, F, S, T>
where
    I: Clone,
    F: Clone,
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            f: self.f.clone(),
            state: self.state.clone(),
            _marker: PhantomData,
        }
    }
}

impl<I, F, S, T> Service<HttpContext, Incoming> for FromFn<I, F, S, T>
where
    I: Service<HttpContext, Incoming, Response = Response, Error = Infallible>
        + Clone
        + Send
        + Sync
        + 'static,
    F: for<'r> MiddlewareHandlerFromFn<'r, T, S> + Clone + Sync,
    S: Clone + Sync,
{
    type Response = I::Response;
    type Error = I::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut HttpContext,
        req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        let next = Next {
            inner: DynService::new(self.inner.clone()),
        };
        Ok(
            self.f.call(cx, req, &self.state, next).await, // .into_response()
        )
    }
}

pub struct Next {
    inner: DynService,
}

impl Next {
    pub async fn run(self, cx: &mut HttpContext, req: Incoming) -> Result<Response, Infallible> {
        self.inner.call(cx, req).await
    }
}
