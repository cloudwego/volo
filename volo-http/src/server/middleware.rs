use std::{convert::Infallible, marker::PhantomData};

use hyper::body::Incoming;
use motore::{layer::Layer, service::Service, ServiceExt};

use super::{
    handler::{MiddlewareHandlerFromFn, MiddlewareHandlerMapResponse},
    route::Route,
    IntoResponse,
};
use crate::{context::ServerContext, request::ServerRequest, response::ServerResponse};

pub struct FromFnLayer<F, T, R, B, E, B2, E2> {
    f: F,
    #[allow(clippy::type_complexity)]
    _marker: PhantomData<fn(T, R, B, E, B2, E2)>,
}

impl<F, T, R, B, E, B2, E2> Clone for FromFnLayer<F, T, R, B, E, B2, E2>
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

pub fn from_fn<F, T, R, B, E, B2, E2>(f: F) -> FromFnLayer<F, T, R, B, E, B2, E2> {
    FromFnLayer {
        f,
        _marker: PhantomData,
    }
}

impl<S, F, T, R, B, E, B2, E2> Layer<S> for FromFnLayer<F, T, R, B, E, B2, E2>
where
    S: Service<ServerContext, ServerRequest<B2>, Response = R, Error = E2>
        + Clone
        + Send
        + Sync
        + 'static,
    R: IntoResponse,
    F: Clone,
{
    type Service =
        FromFn<motore::service::MapResponse<S, fn(R) -> ServerResponse>, F, T, B, E, B2, E2>;

    fn layer(self, service: S) -> Self::Service {
        FromFn {
            service: service.map_response(IntoResponse::into_response),
            f: self.f.clone(),
            _marker: PhantomData,
        }
    }
}

pub struct FromFn<S, F, T, B, E, B2, E2> {
    service: S,
    f: F,
    #[allow(clippy::type_complexity)]
    _marker: PhantomData<fn(T, B, E, B2, E2)>,
}

impl<S, F, T, B, E, B2, E2> Clone for FromFn<S, F, T, B, E, B2, E2>
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

impl<S, F, T, B, E, B2, E2> Service<ServerContext, ServerRequest<B>>
    for FromFn<S, F, T, B, E, B2, E2>
where
    S: Service<ServerContext, ServerRequest<B2>, Response = ServerResponse, Error = E2>
        + Clone
        + Send
        + Sync
        + 'static,
    F: for<'r> MiddlewareHandlerFromFn<'r, T, B, E, B2, E2> + Clone + Sync,
    B: Send + 'static,
    E: IntoResponse,
    B2: Send + 'static,
{
    type Response = S::Response;
    type Error = E;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut ServerContext,
        req: ServerRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        let next = Next {
            service: Route::new(self.service.clone()),
        };
        Ok(self.f.handle(cx, req, next).await)
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

impl<S, Req, F, T> Service<ServerContext, Req> for MapResponse<S, F, T>
where
    Req: Send,
    S: Service<ServerContext, Req> + Clone + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
    F: for<'r> MiddlewareHandlerMapResponse<'r, T> + Clone + Sync,
{
    type Response = ServerResponse;
    type Error = S::Error;

    async fn call(&self, cx: &mut ServerContext, req: Req) -> Result<Self::Response, Self::Error> {
        let response = match self.service.call(cx, req).await {
            Ok(resp) => resp.into_response(),
            Err(e) => e.into_response(),
        };

        Ok(self.f.handle(cx, response).await)
    }
}
