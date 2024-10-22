use motore::{layer::Layer, Service};

use crate::{
    context::ServerContext, request::ServerRequest, response::ServerResponse, server::IntoResponse,
};

#[derive(Debug, Clone, Copy)]
pub(crate) enum BodyLimitKind {
    #[allow(dead_code)]
    Disable,
    Limit(usize),
}

/// [`Layer`] for limiting body size
///
/// Currently only supports [`Multipart`](crate::server::utils::multipart::Multipart) extractor.
///
/// See [`BodyLimitLayer::max`] for more details.
#[derive(Clone)]
pub struct BodyLimitLayer {
    kind: BodyLimitKind,
}

impl BodyLimitLayer {
    /// Create a new [`BodyLimitLayer`] with given [`body_limit`].
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
            kind: BodyLimitKind::Limit(body_limit),
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
where
    S: Send + Sync + 'static,
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
#[derive(Clone)]
pub struct BodyLimitService<S> {
    service: S,
    kind: BodyLimitKind,
}

impl<S, B> Service<ServerContext, ServerRequest<B>> for BodyLimitService<S>
where
    S: Service<ServerContext, ServerRequest<B>> + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
    B: Send,
{
    type Response = ServerResponse;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ServerContext,
        mut req: ServerRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        req.extensions_mut().insert(self.kind);
        Ok(self.service.call(cx, req).await?.into_response())
    }
}
