use std::{convert::Infallible, marker::PhantomData, time::Duration};

use hyper::body::Incoming;
use motore::{layer::Layer, service::Service};

use crate::{
    context::ServerContext,
    handler::HandlerWithoutRequest,
    response::{IntoResponse, Response},
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

impl<S, H, R, T> Service<ServerContext, Incoming> for Filter<S, H, R, T>
where
    S: Service<ServerContext, Incoming, Response = Response, Error = Infallible>
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
        req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        match self.handler.clone().handle(cx).await {
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
pub struct TimeoutLayer<H, R, T> {
    duration: Duration,
    handler: H,
    _marker: PhantomData<(R, T)>,
}

impl<H, R, T> TimeoutLayer<H, R, T> {
    pub fn new(duration: Duration, handler: H) -> Self
    where
        H: Send + Sync + 'static,
    {
        Self {
            duration,
            handler,
            _marker: PhantomData,
        }
    }
}

impl<S, H, R, T> Layer<S> for TimeoutLayer<H, R, T>
where
    S: Send + Sync + 'static,
    H: Clone + Send + Sync + 'static,
    R: Sync,
    T: Sync,
{
    type Service = Timeout<S, H, R, T>;

    fn layer(self, inner: S) -> Self::Service {
        Timeout {
            service: inner,
            duration: self.duration,
            handler: self.handler,
            _marker: PhantomData,
        }
    }
}

#[derive(Clone)]
pub struct Timeout<S, H, R, T> {
    service: S,
    duration: Duration,
    handler: H,
    _marker: PhantomData<(R, T)>,
}

impl<S, H, R, T> Service<ServerContext, Incoming> for Timeout<S, H, R, T>
where
    S: Service<ServerContext, Incoming, Response = Response, Error = Infallible>
        + Send
        + Sync
        + 'static,
    H: HandlerWithoutRequest<T, R> + Clone + Send + Sync + 'static,
    R: IntoResponse + Sync,
    T: Sync,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut ServerContext,
        req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        let fut_service = self.service.call(cx, req);
        let fut_timeout = tokio::time::sleep(self.duration);

        tokio::select! {
            resp = fut_service => resp,
            _ = fut_timeout => {
                Ok(self.handler.clone().handle(cx).await.into_response())
            },
        }
    }
}
