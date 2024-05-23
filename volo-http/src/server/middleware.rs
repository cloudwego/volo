use std::{convert::Infallible, marker::PhantomData};

use hyper::body::Incoming;
use motore::{layer::Layer, service::Service};

use super::{
    handler::{MiddlewareHandlerFromFn, MiddlewareHandlerMapResponse},
    route::Route,
    IntoResponse,
};
use crate::{context::ServerContext, request::ServerRequest, response::ServerResponse};

pub struct FromFnLayer<F, T, B, B2, E2> {
    f: F,
    #[allow(clippy::type_complexity)]
    _marker: PhantomData<fn(T, B, B2, E2)>,
}

impl<F, T, B, B2, E2> Clone for FromFnLayer<F, T, B, B2, E2>
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

pub fn from_fn<F, T, B, B2, E2>(f: F) -> FromFnLayer<F, T, B, B2, E2> {
    FromFnLayer {
        f,
        _marker: PhantomData,
    }
}

impl<S, F, T, B, B2, E2> Layer<S> for FromFnLayer<F, T, B, B2, E2>
where
    S: Service<ServerContext, ServerRequest<B2>, Response = ServerResponse, Error = E2>
        + Clone
        + Send
        + Sync
        + 'static,
    F: Clone,
{
    type Service = FromFn<S, F, T, B, B2, E2>;

    fn layer(self, service: S) -> Self::Service {
        FromFn {
            service,
            f: self.f.clone(),
            _marker: PhantomData,
        }
    }
}

pub struct FromFn<S, F, T, B, B2, E2> {
    service: S,
    f: F,
    _marker: PhantomData<fn(T, B, B2, E2)>,
}

impl<S, F, T, B, B2, E2> Clone for FromFn<S, F, T, B, B2, E2>
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

impl<S, F, T, B, B2, E2> Service<ServerContext, ServerRequest<B>> for FromFn<S, F, T, B, B2, E2>
where
    S: Service<ServerContext, ServerRequest<B2>, Response = ServerResponse, Error = E2>
        + Clone
        + Send
        + Sync
        + 'static,
    F: for<'r> MiddlewareHandlerFromFn<'r, T, B, B2, E2> + Sync,
    B: Send,
    B2: 'static,
{
    type Response = ServerResponse;
    type Error = Infallible;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        let next = Next {
            service: Route::new(self.service.clone()),
        };
        Ok(self.f.handle(cx, req, next).await.into_response())
    }
}

pub struct Next<B = Incoming, E = Infallible> {
    service: Route<B, E>,
}

impl<B, E> Next<B, E> {
    pub async fn run(
        self,
        cx: &mut ServerContext,
        req: ServerRequest<B>,
    ) -> Result<ServerResponse, E> {
        self.service.call(cx, req).await
    }
}

pub struct MapResponseLayer<F, T, R1, R2> {
    f: F,
    _marker: PhantomData<fn(T, R1, R2)>,
}

impl<F, T, R1, R2> Clone for MapResponseLayer<F, T, R1, R2>
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

pub fn map_response<F, T, R1, R2>(f: F) -> MapResponseLayer<F, T, R1, R2> {
    MapResponseLayer {
        f,
        _marker: PhantomData,
    }
}

impl<S, F, T, R1, R2> Layer<S> for MapResponseLayer<F, T, R1, R2>
where
    F: Clone,
{
    type Service = MapResponse<S, F, T, R1, R2>;

    fn layer(self, service: S) -> Self::Service {
        MapResponse {
            service,
            f: self.f.clone(),
            _marker: self._marker,
        }
    }
}

pub struct MapResponse<S, F, T, R1, R2> {
    service: S,
    f: F,
    _marker: PhantomData<fn(T, R1, R2)>,
}

impl<S, F, T, R1, R2> Clone for MapResponse<S, F, T, R1, R2>
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

impl<S, F, T, Req, R1, R2> Service<ServerContext, Req> for MapResponse<S, F, T, R1, R2>
where
    S: Service<ServerContext, Req, Response = R1> + Clone + Send + Sync,
    F: for<'r> MiddlewareHandlerMapResponse<'r, T, R1, R2> + Clone + Sync,
    Req: Send,
{
    type Response = R2;
    type Error = S::Error;

    async fn call(&self, cx: &mut ServerContext, req: Req) -> Result<Self::Response, Self::Error> {
        let resp = self.service.call(cx, req).await?;

        Ok(self.f.handle(cx, resp).await)
    }
}
