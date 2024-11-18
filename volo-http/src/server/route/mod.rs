//! Route module for routing path to [`Service`]s or handlers.
//!
//! This module includes [`Router`], [`MethodRouter`] and [`Route`]. The call path is:
//!
//! `Router` -> `MethodRouter` -> `Route`.
//!
//! [`Router`] is the main router for routing path (uri) to [`MethodRouter`]s. [`MethodRouter`] is
//! a router for routing method (GET, POST, ...) to [`Route`]s. [`Route`] is a handler or service
//! for handling the request.

use std::{convert::Infallible, future::Future, marker::PhantomData};

use http::status::StatusCode;
use motore::{layer::Layer, service::Service, ServiceExt};

use super::{handler::Handler, IntoResponse};
use crate::{body::Body, context::ServerContext, request::Request, response::Response};

pub mod method_router;
pub mod router;
mod utils;

pub use self::{method_router::*, router::Router};

/// The route service used for [`Router`].
pub struct Route<B = Body, E = Infallible> {
    inner: motore::service::BoxService<ServerContext, Request<B>, Response, E>,
}

impl<B, E> Route<B, E> {
    /// Create a new [`Route`] from a [`Service`].
    pub fn new<S>(inner: S) -> Self
    where
        S: Service<ServerContext, Request<B>, Response = Response, Error = E>
            + Send
            + Sync
            + 'static,
        B: 'static,
    {
        Self {
            inner: motore::service::BoxService::new(inner),
        }
    }
}

impl<B, E> Service<ServerContext, Request<B>> for Route<B, E> {
    type Response = Response;
    type Error = E;

    fn call(
        &self,
        cx: &mut ServerContext,
        req: Request<B>,
    ) -> impl Future<Output = Result<Self::Response, Self::Error>> + Send {
        self.inner.call(cx, req)
    }
}

enum Fallback<B = Body, E = Infallible> {
    Route(Route<B, E>),
}

impl<B, E> Service<ServerContext, Request<B>> for Fallback<B, E>
where
    B: Send,
{
    type Response = Response;
    type Error = E;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: Request<B>,
    ) -> Result<Self::Response, Self::Error> {
        match self {
            Self::Route(route) => route.call(cx, req).await,
        }
    }
}

impl<B, E> Fallback<B, E>
where
    B: Send + 'static,
    E: 'static,
{
    fn from_status_code(status: StatusCode) -> Self {
        Self::from_service(RouteForStatusCode::new(status))
    }

    fn from_handler<H, T>(handler: H) -> Self
    where
        H: Handler<T, B, E> + Clone + Send + Sync + 'static,
        T: 'static,
    {
        Self::from_service(handler.into_service())
    }

    fn from_service<S>(service: S) -> Self
    where
        S: Service<ServerContext, Request<B>, Error = E> + Send + Sync + 'static,
        S::Response: IntoResponse,
    {
        Self::Route(Route::new(
            service.map_response(IntoResponse::into_response),
        ))
    }

    fn map<F, B2, E2>(self, f: F) -> Fallback<B2, E2>
    where
        F: FnOnce(Route<B, E>) -> Route<B2, E2> + Clone + 'static,
    {
        match self {
            Self::Route(route) => Fallback::Route(f(route)),
        }
    }

    fn layer<L, B2, E2>(self, l: L) -> Fallback<B2, E2>
    where
        L: Layer<Route<B, E>> + Clone + Send + Sync + 'static,
        L::Service: Service<ServerContext, Request<B2>, Error = E2> + Send + Sync + 'static,
        <L::Service as Service<ServerContext, Request<B2>>>::Response: IntoResponse,
        B2: 'static,
    {
        self.map(move |route: Route<B, E>| {
            Route::new(
                l.clone()
                    .layer(route)
                    .map_response(IntoResponse::into_response),
            )
        })
    }
}

struct RouteForStatusCode<B, E> {
    status: StatusCode,
    _marker: PhantomData<fn(B, E)>,
}

impl<B, E> Clone for RouteForStatusCode<B, E> {
    fn clone(&self) -> Self {
        Self {
            status: self.status,
            _marker: self._marker,
        }
    }
}

impl<B, E> RouteForStatusCode<B, E> {
    fn new(status: StatusCode) -> Self {
        Self {
            status,
            _marker: PhantomData,
        }
    }
}

impl<B, E> Service<ServerContext, Request<B>> for RouteForStatusCode<B, E>
where
    B: Send,
{
    type Response = Response;
    type Error = E;

    async fn call(
        &self,
        _: &mut ServerContext,
        _: Request<B>,
    ) -> Result<Self::Response, Self::Error> {
        Ok(self.status.into_response())
    }
}
