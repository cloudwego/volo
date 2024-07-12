//! Server middleware utilities
use std::{convert::Infallible, marker::PhantomData, sync::Arc};

use hyper::body::Incoming;
use motore::{layer::Layer, service::Service};

use super::{
    handler::{MiddlewareHandlerFromFn, MiddlewareHandlerMapResponse},
    route::Route,
    IntoResponse,
};
use crate::{context::ServerContext, request::ServerRequest, response::ServerResponse};

/// A [`Layer`] from an async function
///
/// This layer is created with [`from_fn`], see that function for more details.
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

/// Create a middleware from an async function
///
/// The function must have the three params `&mut ServerContext`, `ServerRequest` and [`Next`],
/// and the three params must be at the end of function.
///
/// There can also be some other types that implement
/// [`FromContext`](crate::server::extract::FromContext) before the above three params.
///
/// # Examples
///
/// Without any extra params:
///
/// ```
/// use volo_http::{
///     context::ServerContext,
///     request::ServerRequest,
///     response::ServerResponse,
///     server::{
///         middleware::{from_fn, Next},
///         response::IntoResponse,
///         route::{get, Router},
///     },
/// };
///
/// /// Caculate cost of inner services and print it
/// async fn tracer(cx: &mut ServerContext, req: ServerRequest, next: Next) -> ServerResponse {
///     let start = std::time::Instant::now();
///     let resp = next.run(cx, req).await.into_response();
///     let elapsed = start.elapsed();
///     println!("request cost: {elapsed:?}");
///     resp
/// }
///
/// async fn handler() -> &'static str {
///     "Hello, World"
/// }
///
/// let router: Router = Router::new()
///     .route("/", get(handler))
///     .layer(from_fn(tracer));
/// ```
///
/// With params that implement `FromContext`:
///
/// ```
/// use http::{status::StatusCode, uri::Uri};
/// use volo::context::Context;
/// use volo_http::{
///     context::ServerContext,
///     cookie::CookieJar,
///     request::ServerRequest,
///     response::ServerResponse,
///     server::{
///         middleware::{from_fn, Next},
///         response::IntoResponse,
///         route::{get, Router},
///     },
/// };
///
/// struct Session;
///
/// fn check_session(session: &str) -> Option<Session> {
///     unimplemented!()
/// }
///
/// async fn cookies_check(
///     uri: Uri,
///     cookies: CookieJar,
///     cx: &mut ServerContext,
///     req: ServerRequest,
///     next: Next,
/// ) -> Result<ServerResponse, StatusCode> {
///     let session = cookies.get("session");
///     // User is not logged in, and not try to login.
///     if uri.path() != "/api/v1/login" && session.is_none() {
///         return Err(StatusCode::FORBIDDEN);
///     }
///     let session = session.unwrap().value().to_string();
///     let Some(session) = check_session(&session) else {
///         return Err(StatusCode::FORBIDDEN);
///     };
///     cx.extensions_mut().insert(session);
///     Ok(next.run(cx, req).await.into_response())
/// }
///
/// async fn handler() -> &'static str {
///     "Hello, World"
/// }
///
/// let router: Router = Router::new()
///     .route("/", get(handler))
///     .layer(from_fn(cookies_check));
/// ```
///
/// There are some advanced uses of this function, for example, we can convert types of request and
/// error, for example:
///
/// ```
/// use std::convert::Infallible;
///
/// use motore::service::service_fn;
/// use volo_http::{
///     body::BodyConversion,
///     context::ServerContext,
///     request::ServerRequest,
///     response::ServerResponse,
///     server::{
///         middleware::{from_fn, Next},
///         response::IntoResponse,
///         route::{get, get_service, Router},
///     },
/// };
///
/// async fn converter(
///     cx: &mut ServerContext,
///     req: ServerRequest,
///     next: Next<String>,
/// ) -> ServerResponse {
///     let (parts, body) = req.into_parts();
///     let s = body.into_string().await.unwrap();
///     let req = ServerRequest::from_parts(parts, s);
///     next.run(cx, req).await.into_response()
/// }
///
/// async fn service(
///     cx: &mut ServerContext,
///     req: ServerRequest<String>,
/// ) -> Result<ServerResponse, Infallible> {
///     unimplemented!()
/// }
///
/// let router: Router = Router::new()
///     .route("/", get_service(service_fn(service)))
///     .layer(from_fn(converter));
/// ```
pub fn from_fn<F, T, B, B2, E2>(f: F) -> FromFnLayer<F, T, B, B2, E2> {
    FromFnLayer {
        f,
        _marker: PhantomData,
    }
}

impl<S, F, T, B, B2, E2> Layer<S> for FromFnLayer<F, T, B, B2, E2>
where
    S: Service<ServerContext, ServerRequest<B2>, Response = ServerResponse, Error = E2>
        + Send
        + Sync
        + 'static,
{
    type Service = FromFn<Arc<S>, F, T, B, B2, E2>;

    fn layer(self, service: S) -> Self::Service {
        FromFn {
            service: Arc::new(service),
            f: self.f,
            _marker: PhantomData,
        }
    }
}

/// [`Service`] implementation from [`FromFnLayer`]
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

/// Wrapper for inner [`Service`]
///
/// Call [`Next::run`] with context and request for calling the inner [`Service`] and get the
/// response.
///
/// See [`from_fn`] for more details.
pub struct Next<B = Incoming, E = Infallible> {
    service: Route<B, E>,
}

impl<B, E> Next<B, E> {
    /// Call the inner [`Service`]
    pub async fn run(
        self,
        cx: &mut ServerContext,
        req: ServerRequest<B>,
    ) -> Result<ServerResponse, E> {
        self.service.call(cx, req).await
    }
}

/// A [`Layer`] for mapping a response
///
/// This layer is created with [`map_response`], see that function for more details.
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

/// Create a middleware for mapping a response from an async function
///
/// The async function can be:
///
/// - `async fn func(resp: ServerResponse) -> impl IntoResponse`
/// - `async fn func(cx: &mut ServerContext, resp: ServerResponse) -> impl IntoResponse`
///
/// # Examples
///
/// Append some headers:
///
/// ```
/// use volo_http::{
///     response::ServerResponse,
///     server::{
///         middleware::map_response,
///         route::{get, Router},
///     },
/// };
///
/// async fn handler() -> &'static str {
///     "Hello, World"
/// }
///
/// async fn append_header(resp: ServerResponse) -> ((&'static str, &'static str), ServerResponse) {
///     (("Server", "nginx"), resp)
/// }
///
/// let router: Router = Router::new()
///     .route("/", get(handler))
///     .layer(map_response(append_header));
/// ```
pub fn map_response<F, T, R1, R2>(f: F) -> MapResponseLayer<F, T, R1, R2> {
    MapResponseLayer {
        f,
        _marker: PhantomData,
    }
}

impl<S, F, T, R1, R2> Layer<S> for MapResponseLayer<F, T, R1, R2> {
    type Service = MapResponse<S, F, T, R1, R2>;

    fn layer(self, service: S) -> Self::Service {
        MapResponse {
            service,
            f: self.f,
            _marker: self._marker,
        }
    }
}

/// [`Service`] implementation from [`MapResponseLayer`]
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
    S: Service<ServerContext, Req, Response = R1> + Send + Sync,
    F: for<'r> MiddlewareHandlerMapResponse<'r, T, R1, R2> + Sync,
    Req: Send,
{
    type Response = R2;
    type Error = S::Error;

    async fn call(&self, cx: &mut ServerContext, req: Req) -> Result<Self::Response, Self::Error> {
        let resp = self.service.call(cx, req).await?;

        Ok(self.f.handle(cx, resp).await)
    }
}
