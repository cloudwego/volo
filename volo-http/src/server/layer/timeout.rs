use std::time::Duration;

use motore::{layer::Layer, Service};

use crate::{context::ServerContext, request::Request, response::Response, server::IntoResponse};

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
    fn call(self, cx: &'r ServerContext) -> Response;
}

impl<'r, F, R> TimeoutHandler<'r> for F
where
    F: FnOnce(&'r ServerContext) -> R + 'r,
    R: IntoResponse + 'r,
{
    fn call(self, cx: &'r ServerContext) -> Response {
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

impl<S, B, H> Service<ServerContext, Request<B>> for Timeout<S, H>
where
    S: Service<ServerContext, Request<B>> + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
    B: Send,
    H: for<'r> TimeoutHandler<'r> + Clone + Sync,
{
    type Response = Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: Request<B>,
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

#[cfg(test)]
mod timeout_tests {
    use http::{Method, StatusCode};
    use motore::{layer::Layer, Service};

    use crate::{
        body::BodyConversion,
        context::ServerContext,
        server::{
            route::{get, Route},
            test_helpers::empty_cx,
        },
        utils::test_helpers::simple_req,
    };

    #[tokio::test]
    async fn test_timeout_layer() {
        use std::time::Duration;

        use crate::server::layer::TimeoutLayer;

        async fn index_handler() -> &'static str {
            "Hello, World"
        }

        async fn index_timeout_handler() -> &'static str {
            tokio::time::sleep(Duration::from_secs_f64(1.5)).await;
            "Hello, World"
        }

        fn timeout_handler(_: &ServerContext) -> StatusCode {
            StatusCode::REQUEST_TIMEOUT
        }

        let timeout_layer = TimeoutLayer::new(Duration::from_secs(1), timeout_handler);

        let mut cx = empty_cx();

        // Test case 1: timeout
        let route: Route<&str> = Route::new(get(index_timeout_handler));
        let service = timeout_layer.clone().layer(route);
        let req = simple_req(Method::GET, "/", "");
        let resp = service.call(&mut cx, req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);

        // Test case 2: not timeout
        let route: Route<&str> = Route::new(get(index_handler));
        let service = timeout_layer.clone().layer(route);
        let req = simple_req(Method::GET, "/", "");
        let resp = service.call(&mut cx, req).await.unwrap();
        assert_eq!(
            resp.into_body().into_string().await.unwrap(),
            "Hello, World"
        );
    }
}
