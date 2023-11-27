use std::time::Duration;

use http::{Method, Request, Response, StatusCode};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use motore::{layer::Layer, service::Service};

use crate::{
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
pub struct TimeoutLayer<F> {
    duration: Duration,
    handler: F,
}

impl<F> TimeoutLayer<F> {
    pub fn new<T>(duration: Duration, handler: F) -> Self
    where
        F: FnOnce(&HttpContext) -> T + Clone + Sync,
        T: IntoResponse,
    {
        Self { duration, handler }
    }
}

impl<S, F, T> Layer<S> for TimeoutLayer<F>
where
    S: Service<HttpContext, Incoming, Response = Response<RespBody>> + Send + Sync + 'static,
    F: FnOnce(&HttpContext) -> T + Clone + Sync,
    T: IntoResponse,
{
    type Service = Timeout<S, F>;

    fn layer(self, inner: S) -> Self::Service {
        Timeout {
            service: inner,
            duration: self.duration,
            handler: self.handler,
        }
    }
}

#[derive(Clone)]
pub struct Timeout<S, F> {
    service: S,
    duration: Duration,
    handler: F,
}

impl<S, F, T> Service<HttpContext, Incoming> for Timeout<S, F>
where
    S: Service<HttpContext, Incoming, Response = Response<RespBody>> + Send + Sync + 'static,
    F: FnOnce(&HttpContext) -> T + Clone + Sync,
    T: IntoResponse,
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
                Ok((self.handler.clone())(cx).into_response())
            },
        }
    }
}
