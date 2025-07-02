use std::marker::PhantomData;

use motore::{Service, layer::Layer};

use crate::{
    context::ServerContext,
    request::Request,
    response::Response,
    server::{IntoResponse, handler::HandlerWithoutRequest},
};

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
    ///     route::{Router, get},
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

impl<S, B, H, R, T> Service<ServerContext, Request<B>> for Filter<S, H, R, T>
where
    S: Service<ServerContext, Request<B>> + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
    B: Send,
    H: HandlerWithoutRequest<T, Result<(), R>> + Clone + Send + Sync + 'static,
    R: IntoResponse + Send + Sync,
    T: Sync,
{
    type Response = Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: Request<B>,
    ) -> Result<Self::Response, Self::Error> {
        let (mut parts, body) = req.into_parts();
        let res = self.handler.clone().handle(cx, &mut parts).await;
        let req = Request::from_parts(parts, body);
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
                tracing::warn!("[Volo-HTTP] FilterLayer: something wrong while extracting");
                Ok(rej.into_response())
            }
        }
    }
}

#[cfg(test)]
mod filter_tests {
    use http::{Method, StatusCode};
    use motore::{Service, layer::Layer};

    use crate::{
        body::BodyConversion,
        server::{
            route::{Route, any},
            test_helpers::empty_cx,
        },
        utils::test_helpers::simple_req,
    };

    #[tokio::test]
    async fn test_filter_layer() {
        use crate::server::layer::FilterLayer;

        async fn reject_post(method: Method) -> Result<(), StatusCode> {
            if method == Method::POST {
                Err(StatusCode::METHOD_NOT_ALLOWED)
            } else {
                Ok(())
            }
        }

        async fn handler() -> &'static str {
            "Hello, World"
        }

        let filter_layer = FilterLayer::new(reject_post);
        let route: Route<&str> = Route::new(any(handler));
        let service = filter_layer.layer(route);

        let mut cx = empty_cx();

        // Test case 1: not filter
        let req = simple_req(Method::GET, "/", "");
        let resp = service.call(&mut cx, req).await.unwrap();
        assert_eq!(
            resp.into_body().into_string().await.unwrap(),
            "Hello, World"
        );

        // Test case 2: filter
        let req = simple_req(Method::POST, "/", "");
        let resp = service.call(&mut cx, req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}
