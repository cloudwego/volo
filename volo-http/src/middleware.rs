use std::{convert::Infallible, marker::PhantomData};

use motore::{layer::Layer, service::Service, ServiceExt};

use crate::{
    context::ServerContext,
    handler::{MiddlewareHandlerFromFn, MiddlewareHandlerMapResponse},
    request::ServerRequest,
    response::{IntoResponse, ServerResponse},
    route::Route,
};

pub struct FromFnLayer<F, T, R, E> {
    f: F,
    _marker: PhantomData<fn(T, R, E)>,
}

impl<F, T, R, E> Clone for FromFnLayer<F, T, R, E>
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

pub fn from_fn<F, T, R, E>(f: F) -> FromFnLayer<F, T, R, E> {
    FromFnLayer {
        f,
        _marker: PhantomData,
    }
}

impl<S, F, T, R, E> Layer<S> for FromFnLayer<F, T, R, E>
where
    S: Service<ServerContext, ServerRequest, Response = R, Error = E>
        + Clone
        + Send
        + Sync
        + 'static,
    R: IntoResponse,
    F: Clone,
{
    type Service = FromFn<motore::service::MapResponse<S, fn(R) -> ServerResponse>, F, T, E>;

    fn layer(self, service: S) -> Self::Service {
        FromFn {
            service: service.map_response(IntoResponse::into_response),
            f: self.f.clone(),
            _marker: PhantomData,
        }
    }
}

pub struct FromFn<S, F, T, E> {
    service: S,
    f: F,
    _marker: PhantomData<fn(T, E)>,
}

impl<S, F, T, E> Clone for FromFn<S, F, T, E>
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

impl<S, F, T, E> Service<ServerContext, ServerRequest> for FromFn<S, F, T, E>
where
    S: Service<ServerContext, ServerRequest, Response = ServerResponse, Error = E>
        + Clone
        + Send
        + Sync
        + 'static,
    F: for<'r> MiddlewareHandlerFromFn<'r, T, E> + Clone + Sync,
    E: IntoResponse,
{
    type Response = S::Response;
    type Error = E;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut ServerContext,
        req: ServerRequest,
    ) -> Result<Self::Response, Self::Error> {
        let next = Next {
            service: Route::new(self.service.clone()),
        };
        Ok(self.f.handle(cx, req, next).await)
    }
}

pub struct Next<E = Infallible> {
    service: Route<E>,
}

impl<E> Next<E> {
    pub async fn run(
        self,
        cx: &mut ServerContext,
        req: ServerRequest,
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

impl<S, F, T> Service<ServerContext, ServerRequest> for MapResponse<S, F, T>
where
    S: Service<ServerContext, ServerRequest> + Clone + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
    F: for<'r> MiddlewareHandlerMapResponse<'r, T> + Clone + Sync,
{
    type Response = ServerResponse;
    type Error = S::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut ServerContext,
        req: ServerRequest,
    ) -> Result<Self::Response, Self::Error> {
        let response = match self.service.call(cx, req).await {
            Ok(resp) => resp.into_response(),
            Err(e) => e.into_response(),
        };

        Ok(self.f.handle(cx, response).await)
    }
}
