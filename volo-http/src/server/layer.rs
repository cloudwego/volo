use std::{marker::PhantomData, time::Duration};

use motore::{layer::Layer, service::Service};

use super::{handler::HandlerWithoutRequest, IntoResponse};
use crate::{context::ServerContext, request::ServerRequest, response::ServerResponse};

#[derive(Clone)]
pub struct FilterLayer<H, R, T> {
    handler: H,
    _marker: PhantomData<(R, T)>,
}

impl<H, R, T> FilterLayer<H, R, T> {
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

#[derive(Clone)]
pub struct Filter<S, H, R, T> {
    service: S,
    handler: H,
    _marker: PhantomData<(R, T)>,
}

impl<S, H, R, T> Service<ServerContext, ServerRequest> for Filter<S, H, R, T>
where
    S: Service<ServerContext, ServerRequest> + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
    H: HandlerWithoutRequest<T, Result<(), R>> + Clone + Send + Sync + 'static,
    R: IntoResponse + Send + Sync,
    T: Sync,
{
    type Response = ServerResponse;
    type Error = S::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut ServerContext,
        req: ServerRequest,
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

#[derive(Clone)]
pub struct TimeoutLayer<H> {
    duration: Duration,
    handler: H,
}

impl<H> TimeoutLayer<H> {
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

#[derive(Clone)]
pub struct Timeout<S, H> {
    service: S,
    duration: Duration,
    handler: H,
}

impl<S, H> Service<ServerContext, ServerRequest> for Timeout<S, H>
where
    S: Service<ServerContext, ServerRequest> + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
    H: for<'r> TimeoutHandler<'r> + Clone + Sync,
{
    type Response = ServerResponse;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest,
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
