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

impl<S> Service<ServerContext, ServerRequest> for BodyLimitService<S>
where
    S: Service<ServerContext, ServerRequest> + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
{
    type Response = ServerResponse;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest,
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
