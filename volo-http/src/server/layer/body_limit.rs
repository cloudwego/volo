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

impl<S, B> Service<ServerContext, ServerRequest<B>> for BodyLimitService<S>
where
    S: Service<ServerContext, ServerRequest<B>> + Send + Sync + 'static,
    S::Response: IntoResponse,
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
    use motore::{layer::Layer, Service};

    use crate::{
        server::{
            layer::BodyLimitLayer,
            route::{any, Route},
            test_helpers::empty_cx,
        },
        utils::test_helpers::simple_req,
    };

    #[tokio::test]
    async fn test_body_limit() {
        async fn handler() -> &'static str {
            "Hello, World"
        }

        let body_limit_layer = BodyLimitLayer::new(8);
        let route: Route<_> = Route::new(any(handler));
        let service = body_limit_layer.layer(route);

        let mut cx = empty_cx();

        // Test case 1: reject
        let req = simple_req(Method::GET, "/", "111111111".to_string());
        let res = service.call(&mut cx, req).await.unwrap();
        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);

        // Test case 2: not reject
        let req = simple_req(Method::GET, "/", "1".to_string());
        let res = service.call(&mut cx, req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
