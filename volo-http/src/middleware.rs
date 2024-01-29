use std::{convert::Infallible, marker::PhantomData};

use hyper::body::Incoming;
use motore::{layer::Layer, service::Service};

use crate::{
    context::ServerContext,
    handler::{MiddlewareHandlerFromFn, MiddlewareHandlerMapResponse},
    response::{IntoResponse, Response},
    DynService,
};

pub struct FromFnLayer<F, T> {
    f: F,
    _marker: PhantomData<fn(T)>,
}

impl<F, T> Clone for FromFnLayer<F, T>
where
    F: Clone,
{
    fn clone(&self) -> Self {
        Self {
            f: self.f.clone(),
            _marker: self._marker,
        }
    }
}

pub fn from_fn<F, T>(f: F) -> FromFnLayer<F, T> {
    FromFnLayer {
        f,
        _marker: PhantomData,
    }
}

impl<S, F, T> Layer<S> for FromFnLayer<F, T>
where
    F: Clone,
{
    type Service = FromFn<S, F, T>;

    fn layer(self, service: S) -> Self::Service {
        FromFn {
            service,
            f: self.f.clone(),
            _marker: self._marker,
        }
    }
}

pub struct FromFn<S, F, T> {
    service: S,
    f: F,
    _marker: PhantomData<fn(T)>,
}

impl<S, F, T> Clone for FromFn<S, F, T>
where
    S: Clone,
    F: Clone,
{
    fn clone(&self) -> Self {
        Self {
            service: self.service.clone(),
            f: self.f.clone(),
            _marker: self._marker,
        }
    }
}

impl<S, F, T> Service<ServerContext, Incoming> for FromFn<S, F, T>
where
    S: Service<ServerContext, Incoming, Response = Response, Error = Infallible>
        + Clone
        + Send
        + Sync
        + 'static,
    F: for<'r> MiddlewareHandlerFromFn<'r, T> + Clone + Sync,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut ServerContext,
        req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        let next = Next {
            service: DynService::new(self.service.clone()),
        };
        Ok(self.f.handle(cx, req, next).await)
    }
}

pub struct Next {
    service: DynService,
}

impl Next {
    pub async fn run(self, cx: &mut ServerContext, req: Incoming) -> Result<Response, Infallible> {
        self.service.call(cx, req).await
    }
}

pub struct MapResponseLayer<F, T> {
    f: F,
    _marker: PhantomData<fn(T)>,
}

impl<F, T> Clone for MapResponseLayer<F, T>
where
    F: Clone,
{
    fn clone(&self) -> Self {
        Self {
            f: self.f.clone(),
            _marker: self._marker,
        }
    }
}

pub fn map_response<F, T>(f: F) -> MapResponseLayer<F, T> {
    MapResponseLayer {
        f,
        _marker: PhantomData,
    }
}

impl<S, F, T> Layer<S> for MapResponseLayer<F, T>
where
    F: Clone,
{
    type Service = MapResponse<S, F, T>;

    fn layer(self, service: S) -> Self::Service {
        MapResponse {
            service,
            f: self.f.clone(),
            _marker: self._marker,
        }
    }
}

pub struct MapResponse<S, F, T> {
    service: S,
    f: F,
    _marker: PhantomData<fn(T)>,
}

impl<S, F, T> Clone for MapResponse<S, F, T>
where
    S: Clone,
    F: Clone,
{
    fn clone(&self) -> Self {
        Self {
            service: self.service.clone(),
            f: self.f.clone(),
            _marker: self._marker,
        }
    }
}

impl<S, F, T> Service<ServerContext, Incoming> for MapResponse<S, F, T>
where
    S: Service<ServerContext, Incoming, Response = Response, Error = Infallible>
        + Clone
        + Send
        + Sync
        + 'static,
    F: for<'r> MiddlewareHandlerMapResponse<'r, T> + Clone + Sync,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut ServerContext,
        req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        let response = match self.service.call(cx, req).await {
            Ok(resp) => resp,
            Err(e) => e.into_response(),
        };

        Ok(self.f.handle(cx, response).await)
    }
}
