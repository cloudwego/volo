use http::StatusCode;
use http_body::Body;
use motore::{layer::Layer, Service};

use crate::{
    context::ServerContext, request::ServerRequest, response::ServerResponse, server::IntoResponse,
};

/// [`Layer`] for limiting body size
///
/// See [`BodyLimitLayer::new`] for more details.
#[derive(Clone)]
pub struct BodyLimitLayer {
    limit: usize,
}

impl BodyLimitLayer {
    /// Create a new [`BodyLimitLayer`] with given `body_limit`.
    ///
    /// If the Body is larger than the `body_limit`, the request will be rejected.
    ///
    /// # Examples
    ///
    /// ```
    /// use http::StatusCode;
    /// use volo_http::server::{
    ///     layer::BodyLimitLayer,
    ///     route::{post, Router},
    /// };
    ///
    /// async fn handler() -> &'static str {
    ///     "Hello, World"
    /// }
    ///
    /// let router: Router = Router::new()
    ///     .route("/", post(handler))
    ///     .layer(BodyLimitLayer::new(1024)); // limit body size to 1KB
    /// ```
    pub fn new(body_limit: usize) -> Self {
        Self { limit: body_limit }
    }
}

impl<S> Layer<S> for BodyLimitLayer {
    type Service = BodyLimitService<S>;

    fn layer(self, inner: S) -> Self::Service {
        BodyLimitService {
            service: inner,
            limit: self.limit,
        }
    }
}

/// [`BodyLimitLayer`] generated [`Service`]
///
/// See [`BodyLimitLayer`] for more details.
pub struct BodyLimitService<S> {
    service: S,
    limit: usize,
}

impl<S,B> Service<ServerContext, ServerRequest<B>> for BodyLimitService<S>
where
    S: Service<ServerContext, ServerRequest<B>> + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
    B: Body + Send,
{
    type Response = ServerResponse;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        let (parts, body) = req.into_parts();
        // get body size from content length
        if let Some(size) = parts
            .headers
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok().and_then(|s| s.parse::<usize>().ok()))
        {
            if size > self.limit {
                return Ok(StatusCode::PAYLOAD_TOO_LARGE.into_response());
            }
        } else {
            // get body size from stream
            if body.size_hint().lower() > self.limit as u64 {
                return Ok(StatusCode::PAYLOAD_TOO_LARGE.into_response());
            }
        }

        let req = ServerRequest::from_parts(parts, body);
        Ok(self.service.call(cx, req).await?.into_response())
    }
}

#[cfg(test)]
mod tests {
    use http::{Method, StatusCode};
    use motore::layer::Layer;
    use motore::Service;
    use rand::Rng;
    use crate::server::layer::BodyLimitLayer;
    use crate::server::route::{any, Route};
    use crate::server::test_helpers::empty_cx;
    use crate::utils::test_helpers::simple_req;

    #[tokio::test]
    async fn test_body_limit() {
        async fn handler() -> &'static str {
            "Hello, World"
        }

        let body_limit_layer = BodyLimitLayer::new(1024);
        let route: Route<_> = Route::new(any(handler));
        let service = body_limit_layer.layer(route);

        let mut cx = empty_cx();

        // Test case 1: reject
        let mut rng = rand::thread_rng();
        let min_part_size = 4096;
        let mut body: Vec<u8> = vec![0; min_part_size];
        rng.fill(&mut body[..]);
        let req = simple_req(Method::GET, "/", unsafe { String::from_utf8_unchecked(body) });
        let res = service.call(&mut cx, req).await.unwrap();
        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);

        // Test case 2: not reject
        let req = simple_req(Method::GET, "/", "Hello, World".to_string());
        let res = service.call(&mut cx, req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}