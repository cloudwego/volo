use http::StatusCode;
use http_body::Body;
use motore::{layer::Layer, Service};

use crate::{context::ServerContext, request::ServerRequest};
use crate::response::ServerResponse;
use crate::server::IntoResponse;

#[derive(Debug, Clone, Copy)]
pub(crate) enum BodyLimitKind {
    #[allow(dead_code)]
    Disable,
    #[allow(dead_code)]
    Block(usize),
}

/// [`Layer`] for limiting body size
///
/// Get the body size by the priority:
///
/// 1. [`http::header::CONTENT_LENGTH`]
///
/// 2. [`http_body::Body::size_hint()`]
///
/// See [`BodyLimitLayer::max`] for more details.
#[derive(Clone)]
pub struct BodyLimitLayer {
    kind: BodyLimitKind,
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
    ///     .layer(BodyLimitLayer::max(1024)); // limit body size to 1KB
    /// ```
    pub fn max(body_limit: usize) -> Self {
        Self {
            kind: BodyLimitKind::Block(body_limit),
        }
    }

    /// Create a new [`BodyLimitLayer`] with `body_limit` disabled.
    ///
    /// It's unnecessary to use this method, because the `body_limit` is disabled by default.
    #[allow(dead_code)]
    fn disable() -> Self {
        Self {
            kind: BodyLimitKind::Disable,
        }
    }
}

impl<S> Layer<S> for BodyLimitLayer
{
    type Service = BodyLimitService<S>;

    fn layer(self, inner: S) -> Self::Service {
        BodyLimitService {
            service: inner,
            kind: self.kind,
        }
    }
}

/// [`BodyLimitLayer`] generated [`Service`]
///
/// See [`BodyLimitLayer`] for more details.
pub struct BodyLimitService<S> {
    service: S,
    kind: BodyLimitKind,
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
        if let BodyLimitKind::Block(limit) = self.kind {
            // get body size from content length
            if let Some(size) = parts.headers.get(http::header::CONTENT_LENGTH).and_then(|v| v.to_str().ok().and_then(|s| s.parse::<usize>().ok())) {
                if size > limit {
                    return Ok(StatusCode::PAYLOAD_TOO_LARGE.into_response());
                }
            } else {
                // get body size from stream
                if body.size_hint().lower() > limit as u64 {
                    return Ok(StatusCode::PAYLOAD_TOO_LARGE.into_response());
                }
            }
        }

        let mut req = ServerRequest::from_parts(parts, body);
        Ok(self.service.call(cx, req).await?.into_response())
    }
}
