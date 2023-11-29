use std::{marker::PhantomData, time::Duration};

use http::{Method, Request, Response, StatusCode};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use motore::{layer::Layer, service::Service};

use crate::{
    handler::HandlerWithoutRequest,
    response::{IntoResponse, RespBody},
    HttpContext,
};

pub trait LayerExt {
    fn method(
        self,
        method: Method,
    ) -> FilterLayer<Box<dyn Fn(&mut HttpContext, &Request<Incoming>) -> Result<(), StatusCode>>>
    where
        Self: Sized,
    {
        self.filter(Box::new(
            move |cx: &mut HttpContext, _: &Request<Incoming>| {
                if cx.method == method {
                    Ok(())
                } else {
                    Err(StatusCode::METHOD_NOT_ALLOWED)
                }
            },
        ))
    }

    fn filter<F>(self, f: F) -> FilterLayer<F>
    where
        Self: Sized,
        F: Fn(&mut HttpContext, &Request<Incoming>) -> Result<(), StatusCode>,
    {
        FilterLayer { f }
    }
}

pub struct FilterLayer<F> {
    f: F,
}

impl<S, F> Layer<S> for FilterLayer<F>
where
    S: Service<HttpContext, Request<Incoming>, Response = Response<Full<Bytes>>>
        + Send
        + Sync
        + 'static,
    F: Fn(&mut HttpContext, &Request<Incoming>) -> Result<(), StatusCode> + Send + Sync,
{
    type Service = Filter<S, F>;

    fn layer(self, inner: S) -> Self::Service {
        Filter {
            service: inner,
            f: self.f,
        }
    }
}

pub struct Filter<S, F> {
    service: S,
    f: F,
}

impl<S, F> Service<HttpContext, Request<Incoming>> for Filter<S, F>
where
    S: Service<HttpContext, Request<Incoming>, Response = Response<Full<Bytes>>>
        + Send
        + Sync
        + 'static,
    F: Fn(&mut HttpContext, &Request<Incoming>) -> Result<(), StatusCode> + Send + Sync,
{
    type Response = S::Response;

    type Error = S::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut HttpContext,
        req: Request<Incoming>,
    ) -> Result<Self::Response, Self::Error> {
        if let Err(status) = (self.f)(cx, &req) {
            return Ok(Response::builder()
                .status(status)
                .body(Full::new(Bytes::new()))
                .unwrap());
        }
        self.service.call(cx, req).await
    }
}

#[derive(Clone)]
pub struct TimeoutLayer<H, T> {
    duration: Duration,
    handler: H,
    _marker: PhantomData<T>,
}

impl<H, T> TimeoutLayer<H, T> {
    pub fn new(duration: Duration, handler: H) -> Self
    where
        H: HandlerWithoutRequest<T> + Clone + Send + Sync + 'static,
    {
        Self {
            duration,
            handler,
            _marker: PhantomData,
        }
    }
}

impl<S, H, T> Layer<S> for TimeoutLayer<H, T>
where
    S: Service<HttpContext, Incoming, Response = Response<RespBody>> + Send + Sync + 'static,
    H: HandlerWithoutRequest<T> + Clone + Send + Sync + 'static,
    T: Sync,
{
    type Service = Timeout<S, H, T>;

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
pub struct Timeout<S, H, T> {
    service: S,
    duration: Duration,
    handler: H,
    _marker: PhantomData<T>,
}

impl<S, H, T> Service<HttpContext, Incoming> for Timeout<S, H, T>
where
    S: Service<HttpContext, Incoming, Response = Response<RespBody>> + Send + Sync + 'static,
    S::Error: Send,
    H: HandlerWithoutRequest<T> + Clone + Send + Sync + 'static,
    T: Sync,
{
    type Response = S::Response;

    type Error = S::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut HttpContext,
        req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        let fut_service = self.service.call(cx, req);
        let fut_timeout = tokio::time::sleep(self.duration);

        tokio::select! {
            resp = fut_service => resp,
            _ = fut_timeout => {
                Ok(self.handler.clone().call(cx).await.into_response())
            },
        }
    }
}
