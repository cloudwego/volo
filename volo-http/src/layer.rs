use std::{convert::Infallible, marker::PhantomData, time::Duration};

use http::StatusCode;
use motore::{layer::Layer, service::Service};

use crate::{
    context::ServerContext,
    handler::HandlerWithoutRequest,
    request::ServerRequest,
    response::{IntoResponse, ServerResponse},
};

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
    S: Service<ServerContext, ServerRequest, Response = ServerResponse, Error = Infallible>
        + Send
        + Sync
        + 'static,
    H: HandlerWithoutRequest<T, Result<(), R>> + Clone + Send + Sync + 'static,
    R: IntoResponse + Send + Sync,
    T: Sync,
{
    type Response = S::Response;
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
            Ok(Ok(())) => self.service.call(cx, req).await,
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
pub struct TimeoutLayer {
    duration: Duration,
}

impl TimeoutLayer {
    pub fn new(duration: Duration) -> Self {
        Self { duration }
    }
}

impl<S> Layer<S> for TimeoutLayer
where
    S: Send + Sync + 'static,
{
    type Service = Timeout<S>;

    fn layer(self, inner: S) -> Self::Service {
        Timeout {
            service: inner,
            duration: self.duration,
        }
    }
}

#[derive(Clone)]
pub struct Timeout<S> {
    service: S,
    duration: Duration,
}

impl<S> Service<ServerContext, ServerRequest> for Timeout<S>
where
    S: Service<ServerContext, ServerRequest, Response = ServerResponse, Error = Infallible>
        + Send
        + Sync
        + 'static,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut ServerContext,
        req: ServerRequest,
    ) -> Result<Self::Response, Self::Error> {
        let fut_service = self.service.call(cx, req);
        let fut_timeout = tokio::time::sleep(self.duration);

        tokio::select! {
            resp = fut_service => resp,
            _ = fut_timeout => {
                Ok(StatusCode::REQUEST_TIMEOUT.into_response())
            },
        }
    }
}
