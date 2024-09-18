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
/// use http::{header::HeaderMap, status::StatusCode, uri::Uri};
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
/// struct Session;
///
/// fn get_session(headers: &HeaderMap) -> Option<Session> {
///     unimplemented!()
/// }
///
/// async fn cookies_check(
///     uri: Uri,
///     cx: &mut ServerContext,
///     req: ServerRequest,
///     next: Next,
/// ) -> Result<ServerResponse, StatusCode> {
///     // User is not logged in, and not try to login.
///     let session = get_session(req.headers());
///     if uri.path() != "/api/v1/login" && session.is_none() {
///         return Err(StatusCode::FORBIDDEN);
///     }
///     // do something
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

#[cfg(test)]
mod middleware_tests {
    use faststr::FastStr;
    use http::{HeaderValue, Method, Response, StatusCode, Uri};
    use motore::service::service_fn;

    use super::*;
    use crate::{
        body::{Body, BodyConversion},
        context::ServerContext,
        request::ServerRequest,
        response::ServerResponse,
        server::{
            response::IntoResponse,
            route::{any, get_service},
            test_helpers::*,
        },
    };

    async fn print_body_handler(
        _: &mut ServerContext,
        req: ServerRequest<String>,
    ) -> Result<Response<Body>, Infallible> {
        Ok(Response::new(req.into_body().into()))
    }

    async fn append_body_mw(
        cx: &mut ServerContext,
        req: ServerRequest<String>,
        next: Next<String>,
    ) -> ServerResponse {
        let (parts, mut body) = req.into_parts();
        body += "test";
        let req = ServerRequest::from_parts(parts, body);
        next.run(cx, req).await.into_response()
    }

    async fn cors_mw(
        method: Method,
        url: Uri,
        cx: &mut ServerContext,
        req: ServerRequest<String>,
        next: Next<String>,
    ) -> ServerResponse {
        let mut resp = next.run(cx, req).await.into_response();
        resp.headers_mut().insert(
            "Access-Control-Allow-Methods",
            HeaderValue::from_str(method.as_str()).unwrap(),
        );
        resp.headers_mut().insert(
            "Access-Control-Allow-Origin",
            HeaderValue::from_str(url.to_string().as_str()).unwrap(),
        );
        resp.headers_mut().insert(
            "Access-Control-Allow-Headers",
            HeaderValue::from_str("*").unwrap(),
        );
        resp
    }

    #[tokio::test]
    async fn test_from_fn_with_necessary_params() {
        let handler = service_fn(print_body_handler);
        let mut cx = empty_cx();

        let service = from_fn(append_body_mw).layer(handler);
        let req = simple_req(Method::GET, "/", String::from(""));
        let resp = service.call(&mut cx, req).await.unwrap();
        assert_eq!(resp.into_body().into_string().await.unwrap(), "test");

        // Test case 3: Return type [`Result<_,_>`]
        async fn error_mw(
            _: &mut ServerContext,
            _: ServerRequest<String>,
            _: Next<String>,
        ) -> Result<ServerResponse, StatusCode> {
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
        let service = from_fn(error_mw).layer(handler);
        let req = simple_req(Method::GET, "/", String::from("test"));
        let resp = service.call(&mut cx, req).await.unwrap();
        let status = resp.status();
        let (_, body) = resp.into_parts();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.into_string().await.unwrap(), "");
    }

    #[tokio::test]
    async fn test_from_fn_with_optional_params() {
        let handler = service_fn(print_body_handler);
        let mut cx = empty_cx();

        let service = from_fn(cors_mw).layer(handler);
        let req = simple_req(Method::GET, "/", String::from(""));
        let resp = service.call(&mut cx, req).await.unwrap();
        assert_eq!(
            resp.headers().get("Access-Control-Allow-Methods").unwrap(),
            "GET"
        );
        assert_eq!(
            resp.headers().get("Access-Control-Allow-Origin").unwrap(),
            "/"
        );
        assert_eq!(
            resp.headers().get("Access-Control-Allow-Headers").unwrap(),
            "*"
        );
    }

    #[tokio::test]
    async fn test_from_fn_with_multiple_mws() {
        let handler = service_fn(print_body_handler);
        let mut cx = empty_cx();

        let service = from_fn(cors_mw).layer(handler);
        let service = from_fn(append_body_mw).layer(service);
        let req = simple_req(Method::GET, "/", String::from(""));
        let resp = service.call(&mut cx, req).await.unwrap();
        let (parts, body) = resp.into_parts();
        assert_eq!(
            parts.headers.get("Access-Control-Allow-Methods").unwrap(),
            "GET"
        );
        assert_eq!(
            parts.headers.get("Access-Control-Allow-Origin").unwrap(),
            "/"
        );
        assert_eq!(
            parts.headers.get("Access-Control-Allow-Headers").unwrap(),
            "*"
        );
        assert_eq!(body.into_string().await.unwrap(), "test");
    }

    #[tokio::test]
    async fn test_from_fn_converts() {
        async fn converter(
            cx: &mut ServerContext,
            req: ServerRequest<String>,
            next: Next<FastStr>,
        ) -> ServerResponse {
            let (parts, body) = req.into_parts();
            let s = body.into_faststr().await.unwrap();
            let req = ServerRequest::from_parts(parts, s);
            let _: ServerRequest<FastStr> = req;
            next.run(cx, req).await.into_response()
        }

        async fn service(
            _: &mut ServerContext,
            _: ServerRequest<FastStr>,
        ) -> Result<ServerResponse, Infallible> {
            Ok(Response::new(String::from("Hello, World").into()))
        }

        let route = Route::new(get_service(service_fn(service)));
        let service = from_fn(converter).layer(route);

        let _: Result<ServerResponse, Infallible> = service
            .call(
                &mut empty_cx(),
                simple_req(Method::GET, "/", String::from("")),
            )
            .await;
    }

    async fn index_handler() -> &'static str {
        "Hello, World"
    }

    #[tokio::test]
    async fn test_map_response() {
        async fn append_header(
            resp: ServerResponse,
        ) -> ((&'static str, &'static str), ServerResponse) {
            (("Server", "nginx"), resp)
        }

        let route: Route<String> = Route::new(any(index_handler));
        let service = map_response(append_header).layer(route);

        let mut cx = empty_cx();
        let req = simple_req(Method::GET, "/", String::from(""));
        let resp = service.call(&mut cx, req).await.unwrap();
        let (parts, _) = resp.into_response().into_parts();
        assert_eq!(parts.headers.get("Server").unwrap(), "nginx");
    }
}
