//! Collections of some userful [`Layer`]s.
//!
//! See [`FilterLayer`] and [`TimeoutLayer`] for more details.

use std::{marker::PhantomData, time::Duration};

use motore::{layer::Layer, service::Service};

use super::{handler::HandlerWithoutRequest, IntoResponse};
use crate::{context::ServerContext, request::ServerRequest, response::ServerResponse};

/// [`Layer`] for filtering requests
///
/// See [`FilterLayer::new`] for more details.
#[derive(Clone)]
pub struct FilterLayer<H, R, T> {
    handler: H,
    _marker: PhantomData<(R, T)>,
}

impl<H, R, T> FilterLayer<H, R, T> {
    /// Create a new [`FilterLayer`]
    ///
    /// The `handler` is an async function with some params that implement
    /// [`FromContext`](crate::server::extract::FromContext), and returns
    /// `Result<(), impl IntoResponse>`.
    ///
    /// If the handler returns `Ok(())`, the request will proceed. However, if the handler returns
    /// `Err` with an object that implements [`IntoResponse`], the request will be rejected with
    /// the returned object as the response.
    ///
    /// # Examples
    ///
    /// ```
    /// use http::{method::Method, status::StatusCode};
    /// use volo_http::server::{
    ///     layer::FilterLayer,
    ///     route::{get, Router},
    /// };
    ///
    /// async fn reject_post(method: Method) -> Result<(), StatusCode> {
    ///     if method == Method::POST {
    ///         Err(StatusCode::METHOD_NOT_ALLOWED)
    ///     } else {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// async fn handler() -> &'static str {
    ///     "Hello, World"
    /// }
    ///
    /// let router: Router = Router::new()
    ///     .route("/", get(handler))
    ///     .layer(FilterLayer::new(reject_post));
    /// ```
    pub fn new(h: H) -> Self {
        Self {
            handler: h,
            _marker: PhantomData,
        }
    }
}

impl<S, H, R, T> Layer<S> for FilterLayer<H, R, T>
where
    S: Send + Sync + 'static,
    H: Clone + Send + Sync + 'static,
    T: Sync,
{
    type Service = Filter<S, H, R, T>;

    fn layer(self, inner: S) -> Self::Service {
        Filter {
            service: inner,
            handler: self.handler,
            _marker: PhantomData,
        }
    }
}

/// [`FilterLayer`] generated [`Service`]
///
/// See [`FilterLayer`] for more details.
#[derive(Clone)]
pub struct Filter<S, H, R, T> {
    service: S,
    handler: H,
    _marker: PhantomData<(R, T)>,
}

impl<S, B, H, R, T> Service<ServerContext, ServerRequest<B>> for Filter<S, H, R, T>
where
    S: Service<ServerContext, ServerRequest<B>> + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
    B: Send,
    H: HandlerWithoutRequest<T, Result<(), R>> + Clone + Send + Sync + 'static,
    R: IntoResponse + Send + Sync,
    T: Sync,
{
    type Response = ServerResponse;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        let (mut parts, body) = req.into_parts();
        let res = self.handler.clone().handle(cx, &mut parts).await;
        let req = ServerRequest::from_parts(parts, body);
        match res {
            // do not filter it, call the service
            Ok(Ok(())) => self
                .service
                .call(cx, req)
                .await
                .map(IntoResponse::into_response),
            // filter it and return the specified response
            Ok(Err(res)) => Ok(res.into_response()),
            // something wrong while extracting
            Err(rej) => {
                tracing::warn!("[VOLO] FilterLayer: something wrong while extracting");
                Ok(rej.into_response())
            }
        }
    }
}

/// [`Layer`] for setting timeout to the request
///
/// See [`TimeoutLayer::new`] for more details.
#[derive(Clone)]
pub struct TimeoutLayer<H> {
    duration: Duration,
    handler: H,
}

impl<H> TimeoutLayer<H> {
    /// Create a new [`TimeoutLayer`] with given [`Duration`] and handler.
    ///
    /// The handler should be a sync function with [`&ServerContext`](ServerContext) as parameter,
    /// and return anything that implement [`IntoResponse`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use http::status::StatusCode;
    /// use volo_http::{
    ///     context::ServerContext,
    ///     server::{
    ///         layer::TimeoutLayer,
    ///         route::{get, Router},
    ///     },
    /// };
    ///
    /// async fn index() -> &'static str {
    ///     "Hello, World"
    /// }
    ///
    /// fn timeout_handler(_: &ServerContext) -> StatusCode {
    ///     StatusCode::REQUEST_TIMEOUT
    /// }
    ///
    /// let router: Router = Router::new()
    ///     .route("/", get(index))
    ///     .layer(TimeoutLayer::new(Duration::from_secs(1), timeout_handler));
    /// ```
    pub fn new(duration: Duration, handler: H) -> Self {
        Self { duration, handler }
    }
}

impl<S, H> Layer<S> for TimeoutLayer<H>
where
    S: Send + Sync + 'static,
{
    type Service = Timeout<S, H>;

    fn layer(self, inner: S) -> Self::Service {
        Timeout {
            service: inner,
            duration: self.duration,
            handler: self.handler,
        }
    }
}

trait TimeoutHandler<'r> {
    fn call(self, cx: &'r ServerContext) -> ServerResponse;
}

impl<'r, F, R> TimeoutHandler<'r> for F
where
    F: FnOnce(&'r ServerContext) -> R + 'r,
    R: IntoResponse + 'r,
{
    fn call(self, cx: &'r ServerContext) -> ServerResponse {
        self(cx).into_response()
    }
}

/// [`TimeoutLayer`] generated [`Service`]
///
/// See [`TimeoutLayer`] for more details.
#[derive(Clone)]
pub struct Timeout<S, H> {
    service: S,
    duration: Duration,
    handler: H,
}

impl<S, B, H> Service<ServerContext, ServerRequest<B>> for Timeout<S, H>
where
    S: Service<ServerContext, ServerRequest<B>> + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
    B: Send,
    H: for<'r> TimeoutHandler<'r> + Clone + Sync,
{
    type Response = ServerResponse;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        let fut_service = self.service.call(cx, req);
        let fut_timeout = tokio::time::sleep(self.duration);

        tokio::select! {
            resp = fut_service => resp.map(IntoResponse::into_response),
            _ = fut_timeout => {
                Ok((self.handler.clone()).call(cx))
            },
        }
    }
}
